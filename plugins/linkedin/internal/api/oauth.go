package api

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os/exec"
	"runtime"
	"strings"
	"time"
)

const (
	authURL  = "https://www.linkedin.com/oauth/v2/authorization"
	tokenURL = "https://www.linkedin.com/oauth/v2/accessToken"
	scopes   = "openid profile w_member_social"
	callPort = "8484"
	callPath = "/callback"
)

// OAuthResult holds the result of a successful OAuth flow.
type OAuthResult struct {
	AccessToken string
	ExpiresIn   int
	PersonURN   string
	Name        string
	Email       string
}

// RunOAuthFlow starts a local HTTP server, opens the browser for LinkedIn
// authorization, waits for the callback, exchanges the code for tokens,
// and fetches the user profile.
func RunOAuthFlow(clientID, clientSecret string) (*OAuthResult, error) {
	redirectURI := fmt.Sprintf("http://localhost:%s%s", callPort, callPath)

	// Build authorization URL with a random state for CSRF protection.
	stateBytes := make([]byte, 16)
	if _, err := rand.Read(stateBytes); err != nil {
		return nil, fmt.Errorf("generating oauth state: %w", err)
	}
	oauthState := hex.EncodeToString(stateBytes)
	params := url.Values{
		"response_type": {"code"},
		"client_id":     {clientID},
		"redirect_uri":  {redirectURI},
		"scope":         {scopes},
		"state":         {oauthState},
	}
	authorizationURL := authURL + "?" + params.Encode()

	// Channel to receive the auth code from the callback
	codeCh := make(chan string, 1)
	errCh := make(chan error, 1)

	mux := http.NewServeMux()
	mux.HandleFunc(callPath, func(w http.ResponseWriter, r *http.Request) {
		if errParam := r.URL.Query().Get("error"); errParam != "" {
			desc := r.URL.Query().Get("error_description")
			errCh <- fmt.Errorf("OAuth error: %s — %s", errParam, desc)
			fmt.Fprintf(w, "<html><body><h2>Authorization Failed</h2><p>%s: %s</p><p>You can close this tab.</p></body></html>", errParam, desc)
			return
		}

		if returnedState := r.URL.Query().Get("state"); returnedState != oauthState {
			errCh <- fmt.Errorf("OAuth state mismatch: possible CSRF attack")
			fmt.Fprint(w, "<html><body><h2>Error</h2><p>State mismatch. Authorization rejected.</p></body></html>")
			return
		}

		code := r.URL.Query().Get("code")
		if code == "" {
			errCh <- fmt.Errorf("no authorization code in callback")
			fmt.Fprint(w, "<html><body><h2>Error</h2><p>No authorization code received.</p></body></html>")
			return
		}

		codeCh <- code
		fmt.Fprint(w, "<html><body><h2>Authorization Successful</h2><p>You can close this tab and return to the terminal.</p></body></html>")
	})

	listener, err := net.Listen("tcp", ":"+callPort)
	if err != nil {
		return nil, fmt.Errorf("starting local server on port %s: %w", callPort, err)
	}

	server := &http.Server{Handler: mux}
	go func() { _ = server.Serve(listener) }()
	defer func() {
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		defer cancel()
		_ = server.Shutdown(ctx)
	}()

	// Open browser
	fmt.Println("Opening browser for LinkedIn authorization...")
	fmt.Printf("If the browser doesn't open, visit:\n  %s\n\n", authorizationURL)
	openBrowser(authorizationURL)

	fmt.Println("Waiting for authorization callback...")

	// Wait for code or error (with timeout)
	var code string
	select {
	case code = <-codeCh:
	case err := <-errCh:
		return nil, err
	case <-time.After(5 * time.Minute):
		return nil, fmt.Errorf("authorization timed out after 5 minutes")
	}

	fmt.Println("Authorization code received. Exchanging for token...")

	// Exchange code for access token
	tokenResp, err := exchangeCode(code, clientID, clientSecret, redirectURI)
	if err != nil {
		return nil, fmt.Errorf("token exchange failed: %w", err)
	}

	// Fetch user info to get person URN
	client := NewOAuthClient(tokenResp.AccessToken, "")
	info, err := client.GetUserInfo()
	if err != nil {
		return nil, fmt.Errorf("fetching user info: %w", err)
	}

	return &OAuthResult{
		AccessToken: tokenResp.AccessToken,
		ExpiresIn:   tokenResp.ExpiresIn,
		PersonURN:   "urn:li:person:" + info.Sub,
		Name:        info.Name,
		Email:       info.Email,
	}, nil
}

// exchangeCode exchanges an authorization code for an access token.
func exchangeCode(code, clientID, clientSecret, redirectURI string) (*OAuthTokenResponse, error) {
	data := url.Values{
		"grant_type":    {"authorization_code"},
		"code":          {code},
		"redirect_uri":  {redirectURI},
		"client_id":     {clientID},
		"client_secret": {clientSecret},
	}

	resp, err := http.Post(tokenURL, "application/x-www-form-urlencoded", strings.NewReader(data.Encode()))
	if err != nil {
		return nil, fmt.Errorf("requesting token: %w", err)
	}
	defer resp.Body.Close()

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("reading token response: %w", err)
	}

	if resp.StatusCode != http.StatusOK {
		snippet := string(body)
		if len(snippet) > 200 {
			snippet = snippet[:200] + "..."
		}
		return nil, fmt.Errorf("token endpoint returned HTTP %d: %s", resp.StatusCode, snippet)
	}

	var tokenResp OAuthTokenResponse
	if err := json.Unmarshal(body, &tokenResp); err != nil {
		return nil, fmt.Errorf("parsing token response: %w", err)
	}

	if tokenResp.AccessToken == "" {
		return nil, fmt.Errorf("token response missing access_token")
	}

	return &tokenResp, nil
}

// openBrowser opens the default browser to the given URL.
func openBrowser(url string) {
	var cmd *exec.Cmd
	switch runtime.GOOS {
	case "darwin":
		cmd = exec.Command("open", url)
	case "linux":
		cmd = exec.Command("xdg-open", url)
	default:
		return
	}
	_ = cmd.Start()
}
