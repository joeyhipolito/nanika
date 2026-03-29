package daemon

import (
	"crypto"
	"crypto/rsa"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"math/big"
	"net/http"
	"regexp"
	"slices"
	"strings"
	"sync"
	"time"
)

// newAuthMiddleware returns an HTTP middleware that enforces authentication
// based on the configured mode:
//
// LOCAL MODE (APIKey set, CFTeam not set):
//   - Single-factor auth: only bearer token (API key) is required
//   - Used when daemon is bound to localhost only
//
// REMOTE MODE (both APIKey and CFTeam set):
//   - Dual-layer auth: BOTH factors required on every request:
//     Layer 1: Valid API key in "Authorization: Bearer <key>" header
//     Layer 2: Valid CF Access JWT in "Cf-Access-Jwt-Assertion" header
//              with email matching AllowedEmail
//   - API key is checked first (cheap constant-time compare)
//   - JWT verification is deferred (expensive RSA signature validation)
//
// NO-AUTH MODE (neither APIKey nor CFTeam set):
//   - No authentication enforced; all requests allowed
//   - Valid only for localhost-only daemons
//
// The optional verifier parameter allows tests to inject a pre-seeded
// cfJWTVerifier. Pass nil for production use.
func newAuthMiddleware(cfg Config, verifier *cfJWTVerifier) func(http.Handler) http.Handler {
	if cfg.APIKey == "" && cfg.CFTeam == "" {
		return func(next http.Handler) http.Handler { return next }
	}

	if verifier == nil && cfg.CFTeam != "" {
		verifier = newCFJWTVerifier(cfg.CFTeam, cfg.CFAud, cfg.AllowedEmail)
	}

	// Remote mode: both CF Access and API key are configured.
	// Per REMOTE-ACCESS.md and CLOUD-DEPLOYMENT.md, either alone is insufficient.
	remoteMode := cfg.CFTeam != "" && cfg.APIKey != ""

	// Pre-convert API key to bytes once to avoid per-request allocation.
	var apiKeyBytes []byte
	if cfg.APIKey != "" {
		apiKeyBytes = []byte(cfg.APIKey)
	}

	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			// Always allow CORS preflight without auth.
			if r.Method == http.MethodOptions {
				applyCORS(w, r)
				w.WriteHeader(http.StatusNoContent)
				return
			}

			if remoteMode {
				// Dual-layer enforcement: require BOTH factors.

				// Check cheap factor first: API key (constant-time compare).
				key := extractBearerToken(r)
				if key == "" || subtle.ConstantTimeCompare([]byte(key), apiKeyBytes) != 1 {
					http.Error(w, "unauthorized", http.StatusUnauthorized)
					return
				}

				// Expensive factor: CF Access JWT with email binding.
				jwtToken := r.Header.Get("Cf-Access-Jwt-Assertion")
				if jwtToken == "" {
					http.Error(w, "unauthorized", http.StatusUnauthorized)
					return
				}
				if err := verifier.Verify(jwtToken); err != nil {
					http.Error(w, "unauthorized", http.StatusUnauthorized)
					return
				}

				next.ServeHTTP(w, r)
				return
			}

			// Single-factor mode (non-remote): accept whichever is configured.

			if apiKeyBytes != nil {
				key := extractBearerToken(r)
				if key != "" && subtle.ConstantTimeCompare([]byte(key), apiKeyBytes) == 1 {
					next.ServeHTTP(w, r)
					return
				}
			}

			if verifier != nil {
				token := r.Header.Get("Cf-Access-Jwt-Assertion")
				if token != "" {
					if err := verifier.Verify(token); err == nil {
						next.ServeHTTP(w, r)
						return
					}
				}
			}

			http.Error(w, "unauthorized", http.StatusUnauthorized)
		})
	}
}

// extractBearerToken returns the token from "Authorization: Bearer <token>",
// or empty string if the header is missing or malformed.
func extractBearerToken(r *http.Request) string {
	const prefix = "Bearer "
	hdr := r.Header.Get("Authorization")
	if len(hdr) > len(prefix) && hdr[:len(prefix)] == prefix {
		return hdr[len(prefix):]
	}
	return ""
}

// localhostOriginRe matches http://localhost:<port> and http://127.0.0.1:<port>.
var localhostOriginRe = regexp.MustCompile(`^http://(localhost|127\.0\.0\.1)(:\d+)?$`)

// applyCORS sets CORS headers restricted to localhost origins only.
// Only http://localhost:* and http://127.0.0.1:* are allowed.
func applyCORS(w http.ResponseWriter, r *http.Request) {
	origin := r.Header.Get("Origin")
	if origin == "" || !localhostOriginRe.MatchString(origin) {
		// No origin or non-localhost origin: do not set Allow-Origin.
		// Browsers will block the cross-origin request.
		w.Header().Set("Access-Control-Allow-Origin", "http://localhost")
	} else {
		w.Header().Set("Access-Control-Allow-Origin", origin)
	}
	w.Header().Set("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
	w.Header().Set("Access-Control-Allow-Headers",
		"Authorization, Content-Type, Cf-Access-Jwt-Assertion, Last-Event-ID, Cache-Control")
	w.Header().Set("Access-Control-Allow-Credentials", "true")
}

// corsMiddleware wraps a handler to apply CORS headers on every response and
// short-circuit OPTIONS preflight requests. This replaces per-handler applyCORS
// calls and the broken "OPTIONS /" catch-all (Go 1.22 ServeMux won't match
// "OPTIONS /" as a wildcard for paths like "/api/personas").
func corsMiddleware(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		applyCORS(w, r)
		if r.Method == http.MethodOptions {
			w.WriteHeader(http.StatusNoContent)
			return
		}
		next.ServeHTTP(w, r)
	})
}

// ---- CF Access JWT verification ----------------------------------------

const jwksCacheTTL = 5 * time.Minute

// cfJWTVerifier validates CF Access JWTs using the team's JWKS endpoint.
// Public keys are cached for jwksCacheTTL to avoid hitting CF on every request.
type cfJWTVerifier struct {
	teamDomain   string
	aud          string
	allowedEmail string

	mu        sync.Mutex
	keyCache  map[string]*rsa.PublicKey
	cacheTime time.Time
}

func newCFJWTVerifier(teamDomain, aud, allowedEmail string) *cfJWTVerifier {
	return &cfJWTVerifier{
		teamDomain:   teamDomain,
		aud:          aud,
		allowedEmail: allowedEmail,
		keyCache:     make(map[string]*rsa.PublicKey),
	}
}

// Verify validates the RS256 signature, expiry, audience, and email of a CF Access JWT.
func (v *cfJWTVerifier) Verify(token string) error {
	parts := strings.Split(token, ".")
	if len(parts) != 3 {
		return fmt.Errorf("invalid JWT: expected 3 parts, got %d", len(parts))
	}

	// Decode and parse header.
	headerBytes, err := base64.RawURLEncoding.DecodeString(parts[0])
	if err != nil {
		return fmt.Errorf("decoding JWT header: %w", err)
	}
	var header struct {
		Alg string `json:"alg"`
		Kid string `json:"kid"`
	}
	if err := json.Unmarshal(headerBytes, &header); err != nil {
		return fmt.Errorf("parsing JWT header: %w", err)
	}
	if header.Alg != "RS256" {
		return fmt.Errorf("unsupported JWT algorithm %q (expected RS256)", header.Alg)
	}

	// Decode and parse payload.
	payloadBytes, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		return fmt.Errorf("decoding JWT payload: %w", err)
	}
	var claims struct {
		Exp   int64    `json:"exp"`
		Aud   []string `json:"aud"`
		Email string   `json:"email"`
	}
	if err := json.Unmarshal(payloadBytes, &claims); err != nil {
		return fmt.Errorf("parsing JWT payload: %w", err)
	}

	// Validate expiry.
	if time.Now().Unix() > claims.Exp {
		return fmt.Errorf("JWT expired at %d", claims.Exp)
	}

	// Validate audience (always required).
	if !slices.Contains(claims.Aud, v.aud) {
		return fmt.Errorf("JWT audience %v does not contain expected %q", claims.Aud, v.aud)
	}

	// Validate email (always required).
	if claims.Email != v.allowedEmail {
		return fmt.Errorf("JWT email %q does not match allowed email", claims.Email)
	}

	// Fetch and cache the CF public key for this kid.
	pubKey, err := v.getKey(header.Kid)
	if err != nil {
		return fmt.Errorf("getting CF public key for kid %q: %w", header.Kid, err)
	}

	// Verify RS256 signature: sign_input = base64url(header) + "." + base64url(payload).
	signInput := parts[0] + "." + parts[1]
	digest := sha256.Sum256([]byte(signInput))
	sigBytes, err := base64.RawURLEncoding.DecodeString(parts[2])
	if err != nil {
		return fmt.Errorf("decoding JWT signature: %w", err)
	}
	if err := rsa.VerifyPKCS1v15(pubKey, crypto.SHA256, digest[:], sigBytes); err != nil {
		return fmt.Errorf("JWT signature verification failed: %w", err)
	}

	return nil
}

// getKey returns the RSA public key for kid, refreshing the cache if stale.
func (v *cfJWTVerifier) getKey(kid string) (*rsa.PublicKey, error) {
	v.mu.Lock()
	defer v.mu.Unlock()

	if time.Since(v.cacheTime) < jwksCacheTTL {
		if key, ok := v.keyCache[kid]; ok {
			return key, nil
		}
	}

	keys, err := v.fetchJWKS()
	if err != nil {
		return nil, err
	}
	v.keyCache = keys
	v.cacheTime = time.Now()

	key, ok := keys[kid]
	if !ok {
		return nil, fmt.Errorf("no key found for kid %q in CF JWKS", kid)
	}
	return key, nil
}

// fetchJWKS retrieves RSA public keys from the CF Access JWKS endpoint.
func (v *cfJWTVerifier) fetchJWKS() (map[string]*rsa.PublicKey, error) {
	url := fmt.Sprintf("https://%s/cdn-cgi/access/certs", v.teamDomain)
	resp, err := http.Get(url) //nolint:gosec — URL is admin-configured
	if err != nil {
		return nil, fmt.Errorf("fetching JWKS from %s: %w", url, err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("JWKS endpoint %s returned %d", url, resp.StatusCode)
	}

	var jwks struct {
		Keys []struct {
			Kid string `json:"kid"`
			Kty string `json:"kty"`
			N   string `json:"n"`
			E   string `json:"e"`
		} `json:"keys"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&jwks); err != nil {
		return nil, fmt.Errorf("decoding JWKS: %w", err)
	}

	keys := make(map[string]*rsa.PublicKey, len(jwks.Keys))
	for _, k := range jwks.Keys {
		if k.Kty != "RSA" {
			continue
		}
		nBytes, err := base64.RawURLEncoding.DecodeString(k.N)
		if err != nil {
			continue
		}
		eBytes, err := base64.RawURLEncoding.DecodeString(k.E)
		if err != nil {
			continue
		}
		n := new(big.Int).SetBytes(nBytes)
		e := new(big.Int).SetBytes(eBytes)
		keys[k.Kid] = &rsa.PublicKey{N: n, E: int(e.Int64())}
	}
	return keys, nil
}
