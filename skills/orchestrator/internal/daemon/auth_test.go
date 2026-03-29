package daemon

import (
	"crypto"
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/event"
)

// ---- test JWT helpers ---------------------------------------------------

// testKey is an RSA key pair generated once per test binary for signing JWTs.
var testKey *rsa.PrivateKey

func init() {
	var err error
	testKey, err = rsa.GenerateKey(rand.Reader, 2048)
	if err != nil {
		panic("generating test RSA key: " + err.Error())
	}
}

const testKid = "test-kid-1"

// signTestJWT creates a minimal RS256 JWT with the given claims, signed by testKey.
func signTestJWT(t *testing.T, claims map[string]any) string {
	t.Helper()

	header := map[string]string{"alg": "RS256", "kid": testKid}
	headerJSON, _ := json.Marshal(header)
	claimsJSON, _ := json.Marshal(claims)

	headerB64 := base64.RawURLEncoding.EncodeToString(headerJSON)
	claimsB64 := base64.RawURLEncoding.EncodeToString(claimsJSON)

	signInput := headerB64 + "." + claimsB64
	digest := sha256.Sum256([]byte(signInput))
	sig, err := rsa.SignPKCS1v15(rand.Reader, testKey, crypto.SHA256, digest[:])
	if err != nil {
		t.Fatalf("signing test JWT: %v", err)
	}

	return signInput + "." + base64.RawURLEncoding.EncodeToString(sig)
}

// validClaims returns JWT claims that pass all verifier checks.
func validClaims(email, aud string) map[string]any {
	return map[string]any{
		"exp":   time.Now().Add(time.Hour).Unix(),
		"aud":   []string{aud},
		"email": email,
	}
}

// seedVerifier pre-populates the verifier's key cache with testKey's public key
// so tests don't need a real JWKS HTTP endpoint.
func seedVerifier(v *cfJWTVerifier) {
	v.mu.Lock()
	defer v.mu.Unlock()
	v.keyCache[testKid] = &testKey.PublicKey
	v.cacheTime = time.Now()
}

// newRemoteTestServer returns an APIServer with dual-layer auth enabled
// and a pre-seeded JWT verifier. Uses the real newAuthMiddleware via
// verifier injection — no test-only middleware duplication.
func newRemoteTestServer(t *testing.T, apiKey, allowedEmail, cfAud string) *APIServer {
	t.Helper()
	bus := event.NewBus()
	cfg := Config{
		APIKey:       apiKey,
		CFTeam:       "test.cloudflareaccess.com",
		CFAud:        cfAud,
		AllowedEmail: allowedEmail,
	}

	v := newCFJWTVerifier(cfg.CFTeam, cfg.CFAud, cfg.AllowedEmail)
	seedVerifier(v)

	// Build a minimal server using the real auth middleware with injected verifier.
	srv := &APIServer{bus: bus, cfg: cfg}
	mux := http.NewServeMux()
	auth := newAuthMiddleware(cfg, v)
	mux.Handle("GET /api/missions", auth(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		writeJSON(w, []string{})
	})))
	mux.HandleFunc("GET /api/health", srv.handleHealth)
	srv.srv = &http.Server{Handler: corsMiddleware(mux)}
	return srv
}

// ---- dual-layer auth tests ----------------------------------------------

const (
	testAPIKey = "test-secret-key-abc123"
	testEmail  = "user@example.com"
	testCFAud  = "test-aud-value"
)

func TestDualAuth_BothValid_Allows(t *testing.T) {
	srv := newRemoteTestServer(t, testAPIKey, testEmail, testCFAud)
	jwt := signTestJWT(t, validClaims(testEmail, testCFAud))

	req := httptest.NewRequest(http.MethodGet, "/api/missions", nil)
	req.Header.Set("Authorization", "Bearer "+testAPIKey)
	req.Header.Set("Cf-Access-Jwt-Assertion", jwt)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("both valid: want 200, got %d; body: %s", rec.Code, rec.Body.String())
	}
}

func TestDualAuth_MissingJWT_Rejects(t *testing.T) {
	srv := newRemoteTestServer(t, testAPIKey, testEmail, testCFAud)

	req := httptest.NewRequest(http.MethodGet, "/api/missions", nil)
	req.Header.Set("Authorization", "Bearer "+testAPIKey)
	// No Cf-Access-Jwt-Assertion header.
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("missing JWT: want 401, got %d", rec.Code)
	}
}

func TestDualAuth_MissingAPIKey_Rejects(t *testing.T) {
	srv := newRemoteTestServer(t, testAPIKey, testEmail, testCFAud)
	jwt := signTestJWT(t, validClaims(testEmail, testCFAud))

	req := httptest.NewRequest(http.MethodGet, "/api/missions", nil)
	// No Authorization header.
	req.Header.Set("Cf-Access-Jwt-Assertion", jwt)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("missing API key: want 401, got %d", rec.Code)
	}
}

func TestDualAuth_WrongAPIKey_Rejects(t *testing.T) {
	srv := newRemoteTestServer(t, testAPIKey, testEmail, testCFAud)
	jwt := signTestJWT(t, validClaims(testEmail, testCFAud))

	req := httptest.NewRequest(http.MethodGet, "/api/missions", nil)
	req.Header.Set("Authorization", "Bearer wrong-key")
	req.Header.Set("Cf-Access-Jwt-Assertion", jwt)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("wrong API key: want 401, got %d", rec.Code)
	}
}

func TestDualAuth_InvalidJWT_Rejects(t *testing.T) {
	srv := newRemoteTestServer(t, testAPIKey, testEmail, testCFAud)

	req := httptest.NewRequest(http.MethodGet, "/api/missions", nil)
	req.Header.Set("Authorization", "Bearer "+testAPIKey)
	req.Header.Set("Cf-Access-Jwt-Assertion", "not.a.jwt")
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("invalid JWT: want 401, got %d", rec.Code)
	}
}

func TestDualAuth_ExpiredJWT_Rejects(t *testing.T) {
	srv := newRemoteTestServer(t, testAPIKey, testEmail, testCFAud)
	claims := validClaims(testEmail, testCFAud)
	claims["exp"] = time.Now().Add(-time.Hour).Unix() // expired
	jwt := signTestJWT(t, claims)

	req := httptest.NewRequest(http.MethodGet, "/api/missions", nil)
	req.Header.Set("Authorization", "Bearer "+testAPIKey)
	req.Header.Set("Cf-Access-Jwt-Assertion", jwt)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("expired JWT: want 401, got %d", rec.Code)
	}
}

func TestDualAuth_APIKeyAlone_Insufficient(t *testing.T) {
	// Critical security test: API key alone must NOT grant access
	// when remote mode is enabled.
	srv := newRemoteTestServer(t, testAPIKey, testEmail, testCFAud)

	req := httptest.NewRequest(http.MethodGet, "/api/missions", nil)
	req.Header.Set("Authorization", "Bearer "+testAPIKey)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("API key alone in remote mode: want 401, got %d", rec.Code)
	}
}

func TestDualAuth_JWTAlone_Insufficient(t *testing.T) {
	// JWT alone must NOT grant access when remote mode is enabled.
	srv := newRemoteTestServer(t, testAPIKey, testEmail, testCFAud)
	jwt := signTestJWT(t, validClaims(testEmail, testCFAud))

	req := httptest.NewRequest(http.MethodGet, "/api/missions", nil)
	req.Header.Set("Cf-Access-Jwt-Assertion", jwt)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("JWT alone in remote mode: want 401, got %d", rec.Code)
	}
}

// ---- email binding tests ------------------------------------------------

func TestEmailBinding_WrongEmail_Rejects(t *testing.T) {
	srv := newRemoteTestServer(t, testAPIKey, testEmail, testCFAud)
	// Sign a JWT with a different email.
	jwt := signTestJWT(t, validClaims("attacker@evil.com", testCFAud))

	req := httptest.NewRequest(http.MethodGet, "/api/missions", nil)
	req.Header.Set("Authorization", "Bearer "+testAPIKey)
	req.Header.Set("Cf-Access-Jwt-Assertion", jwt)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("wrong email: want 401, got %d", rec.Code)
	}
}

func TestEmailBinding_EmptyEmail_Rejects(t *testing.T) {
	srv := newRemoteTestServer(t, testAPIKey, testEmail, testCFAud)
	// JWT with no email claim.
	claims := map[string]any{
		"exp": time.Now().Add(time.Hour).Unix(),
		"aud": []string{testCFAud},
	}
	jwt := signTestJWT(t, claims)

	req := httptest.NewRequest(http.MethodGet, "/api/missions", nil)
	req.Header.Set("Authorization", "Bearer "+testAPIKey)
	req.Header.Set("Cf-Access-Jwt-Assertion", jwt)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("empty email: want 401, got %d", rec.Code)
	}
}

func TestEmailBinding_CorrectEmail_Allows(t *testing.T) {
	srv := newRemoteTestServer(t, testAPIKey, testEmail, testCFAud)
	jwt := signTestJWT(t, validClaims(testEmail, testCFAud))

	req := httptest.NewRequest(http.MethodGet, "/api/missions", nil)
	req.Header.Set("Authorization", "Bearer "+testAPIKey)
	req.Header.Set("Cf-Access-Jwt-Assertion", jwt)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("correct email: want 200, got %d", rec.Code)
	}
}

func TestEmailBinding_WrongAudience_Rejects(t *testing.T) {
	srv := newRemoteTestServer(t, testAPIKey, testEmail, testCFAud)
	jwt := signTestJWT(t, validClaims(testEmail, "wrong-aud"))

	req := httptest.NewRequest(http.MethodGet, "/api/missions", nil)
	req.Header.Set("Authorization", "Bearer "+testAPIKey)
	req.Header.Set("Cf-Access-Jwt-Assertion", jwt)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("wrong audience: want 401, got %d", rec.Code)
	}
}

// ---- cfJWTVerifier.Verify unit tests ------------------------------------

func TestVerify_EmailEnforcement(t *testing.T) {
	const enforcedAud = "test-enforcement-aud"

	cases := []struct {
		name         string
		allowedEmail string
		jwtEmail     string
		wantErr      bool
	}{
		{"matching email", "user@example.com", "user@example.com", false},
		{"wrong email", "user@example.com", "attacker@evil.com", true},
		{"empty jwt email", "user@example.com", "", true},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			v := newCFJWTVerifier("test.cloudflareaccess.com", enforcedAud, tc.allowedEmail)
			seedVerifier(v)

			claims := map[string]any{
				"exp": time.Now().Add(time.Hour).Unix(),
				"aud": []string{enforcedAud},
			}
			if tc.jwtEmail != "" {
				claims["email"] = tc.jwtEmail
			}
			jwt := signTestJWT(t, claims)

			err := v.Verify(jwt)
			if tc.wantErr && err == nil {
				t.Error("want error, got nil")
			}
			if !tc.wantErr && err != nil {
				t.Errorf("want no error, got: %v", err)
			}
		})
	}
}

func TestVerify_ExpiredToken(t *testing.T) {
	v := newCFJWTVerifier("test.cloudflareaccess.com", "", "")
	seedVerifier(v)

	claims := map[string]any{
		"exp": time.Now().Add(-time.Minute).Unix(),
		"aud": []string{},
	}
	jwt := signTestJWT(t, claims)

	if err := v.Verify(jwt); err == nil {
		t.Error("expired token: want error, got nil")
	}
}

func TestVerify_WrongAudience(t *testing.T) {
	v := newCFJWTVerifier("test.cloudflareaccess.com", "expected-aud", "")
	seedVerifier(v)

	claims := map[string]any{
		"exp": time.Now().Add(time.Hour).Unix(),
		"aud": []string{"wrong-aud"},
	}
	jwt := signTestJWT(t, claims)

	if err := v.Verify(jwt); err == nil {
		t.Error("wrong audience: want error, got nil")
	}
}

func TestVerify_MalformedToken(t *testing.T) {
	v := newCFJWTVerifier("test.cloudflareaccess.com", "", "")
	seedVerifier(v)

	cases := []struct {
		name  string
		token string
	}{
		{"empty", ""},
		{"one part", "abc"},
		{"two parts", "abc.def"},
		{"invalid base64 header", "!!!.def.ghi"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if err := v.Verify(tc.token); err == nil {
				t.Errorf("%s: want error, got nil", tc.name)
			}
		})
	}
}

// ---- health endpoint skips auth in remote mode --------------------------

func TestDualAuth_HealthBypassesAuth(t *testing.T) {
	srv := newRemoteTestServer(t, testAPIKey, testEmail, testCFAud)

	req := httptest.NewRequest(http.MethodGet, "/api/health", nil)
	// No auth headers.
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("health in remote mode: want 200, got %d", rec.Code)
	}
}

// ---- remote config validation tests ------------------------------------

func TestValidateRemoteConfig_LocalMode(t *testing.T) {
	cfg := Config{SocketPath: "/tmp/test.sock", PIDPath: "/tmp/test.pid", APIAddr: "127.0.0.1:7331"}
	if err := cfg.ValidateRemoteConfig(); err != nil {
		t.Fatalf("local mode: want nil, got %v", err)
	}
}

func TestValidateRemoteConfig_CompleteConfig(t *testing.T) {
	cfg := Config{
		APIKey:       "secret-key",
		CFTeam:       "myteam.cloudflareaccess.com",
		CFAud:        "my-app-audience",
		AllowedEmail: "admin@example.com",
	}
	if err := cfg.ValidateRemoteConfig(); err != nil {
		t.Fatalf("complete remote config: want nil, got %v", err)
	}
}

func TestValidateRemoteConfig_APIKeyAlone_LocalMode(t *testing.T) {
	// API key alone (no CFTeam) is valid for localhost mode.
	cfg := Config{APIKey: "secret-key"}
	if err := cfg.ValidateRemoteConfig(); err != nil {
		t.Fatalf("api-key alone (local mode): want nil, got %v", err)
	}
}

func TestValidateRemoteConfig_PartialRemoteConfig(t *testing.T) {
	// When CFTeam is set, all four fields must be present (remote mode).
	cases := []struct {
		name        string
		cfg         Config
		wantMissing []string
	}{
		{
			name:        "only cf-team set",
			cfg:         Config{CFTeam: "myteam.cloudflareaccess.com"},
			wantMissing: []string{"--api-key", "--cf-aud", "--allowed-email"},
		},
		{
			name:        "cf-team and api-key, missing aud and email",
			cfg:         Config{CFTeam: "myteam.cloudflareaccess.com", APIKey: "secret"},
			wantMissing: []string{"--cf-aud", "--allowed-email"},
		},
		{
			name:        "three of four set, missing email",
			cfg:         Config{CFTeam: "myteam.cloudflareaccess.com", APIKey: "secret", CFAud: "aud"},
			wantMissing: []string{"--allowed-email"},
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := tc.cfg.ValidateRemoteConfig()
			if err == nil {
				t.Fatal("partial remote config: want error, got nil")
			}
			for _, flag := range tc.wantMissing {
				if !strings.Contains(err.Error(), flag) {
					t.Errorf("error %q does not mention missing flag %q", err.Error(), flag)
				}
			}
		})
	}
}

func TestValidateRemoteConfig_InvalidLocalConfig(t *testing.T) {
	// Local mode cannot mix APIKey with CF fields (those require CFTeam).
	cases := []struct {
		name string
		cfg  Config
	}{
		{
			name: "api-key with cf-aud",
			cfg:  Config{APIKey: "secret", CFAud: "aud"},
		},
		{
			name: "api-key with allowed-email",
			cfg:  Config{APIKey: "secret", AllowedEmail: "user@example.com"},
		},
		{
			name: "cf-aud without cf-team",
			cfg:  Config{CFAud: "aud"},
		},
		{
			name: "allowed-email without cf-team",
			cfg:  Config{AllowedEmail: "user@example.com"},
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := tc.cfg.ValidateRemoteConfig()
			if err == nil {
				t.Fatal("invalid local config: want error, got nil")
			}
		})
	}
}

// ---- CORS preflight bypasses auth in remote mode ------------------------

func TestDualAuth_CORSPreflight(t *testing.T) {
	srv := newRemoteTestServer(t, testAPIKey, testEmail, testCFAud)

	req := httptest.NewRequest(http.MethodOptions, "/api/missions", nil)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code == http.StatusUnauthorized {
		t.Fatal("OPTIONS in remote mode: should bypass auth, got 401")
	}
}
