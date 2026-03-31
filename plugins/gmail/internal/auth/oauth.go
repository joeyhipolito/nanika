// Package auth handles OAuth2 authentication for the Gmail CLI.
// It provides a localhost-redirect browser flow and token persistence.
package auth

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"net"
	"net/http"
	"os/exec"
	"runtime"
	"time"

	"golang.org/x/oauth2"
	"golang.org/x/oauth2/google"
	"google.golang.org/api/calendar/v3"
	"google.golang.org/api/drive/v3"
	"google.golang.org/api/gmail/v1"
)

// Scopes defines the OAuth2 scopes requested for Gmail, Calendar, and Drive access.
var Scopes = []string{
	gmail.GmailModifyScope,
	gmail.GmailComposeScope,
	gmail.GmailSendScope,
	gmail.GmailSettingsBasicScope,
	calendar.CalendarScope,
	drive.DriveReadonlyScope,
}

// OAuthConfig creates an oauth2.Config from client credentials.
func OAuthConfig(clientID, clientSecret string) *oauth2.Config {
	return &oauth2.Config{
		ClientID:     clientID,
		ClientSecret: clientSecret,
		Endpoint:     google.Endpoint,
		Scopes:       Scopes,
		RedirectURL:  "http://localhost", // port appended at auth time
	}
}

// Authorize runs the full OAuth2 browser flow and returns a token.
// It starts a local HTTP server, opens the browser for consent,
// waits for the callback (up to 2 minutes), and exchanges the code.
func Authorize(cfg *oauth2.Config) (*oauth2.Token, error) {
	// Find a free port
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return nil, fmt.Errorf("failed to start local server: %w", err)
	}
	port := listener.Addr().(*net.TCPAddr).Port
	cfg.RedirectURL = fmt.Sprintf("http://localhost:%d/callback", port)

	// Generate a random state parameter for CSRF protection
	state, err := randomState()
	if err != nil {
		listener.Close()
		return nil, fmt.Errorf("failed to generate state: %w", err)
	}

	// Channel to receive the authorization code or error
	type authResult struct {
		code string
		err  error
	}
	resultCh := make(chan authResult, 1)

	mux := http.NewServeMux()
	mux.HandleFunc("/callback", func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Query().Get("state") != state {
			resultCh <- authResult{err: fmt.Errorf("state mismatch: possible CSRF attack")}
			http.Error(w, "State mismatch", http.StatusBadRequest)
			return
		}

		if errParam := r.URL.Query().Get("error"); errParam != "" {
			resultCh <- authResult{err: fmt.Errorf("authorization denied: %s", errParam)}
			fmt.Fprintf(w, "<html><body><h1>Authorization denied.</h1><p>You can close this tab.</p></body></html>")
			return
		}

		code := r.URL.Query().Get("code")
		if code == "" {
			resultCh <- authResult{err: fmt.Errorf("no authorization code in callback")}
			http.Error(w, "Missing code", http.StatusBadRequest)
			return
		}

		resultCh <- authResult{code: code}
		fmt.Fprintf(w, "<html><body><h1>Authorization successful!</h1><p>You can close this tab.</p></body></html>")
	})

	server := &http.Server{Handler: mux}

	// Start server in background
	go func() {
		if err := server.Serve(listener); err != nil && err != http.ErrServerClosed {
			resultCh <- authResult{err: fmt.Errorf("callback server error: %w", err)}
		}
	}()

	// Build and open the auth URL
	authURL := cfg.AuthCodeURL(state, oauth2.AccessTypeOffline, oauth2.ApprovalForce)
	fmt.Printf("Opening browser for authorization...\n")
	fmt.Printf("If the browser doesn't open, visit:\n%s\n\n", authURL)

	if err := openBrowser(authURL); err != nil {
		fmt.Printf("Could not open browser automatically: %v\n", err)
	}

	// Wait for callback with timeout
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()

	var result authResult
	select {
	case result = <-resultCh:
	case <-ctx.Done():
		result = authResult{err: fmt.Errorf("authorization timed out after 2 minutes")}
	}

	// Shut down the server
	shutdownCtx, shutdownCancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer shutdownCancel()
	_ = server.Shutdown(shutdownCtx)

	if result.err != nil {
		return nil, result.err
	}

	// Exchange the authorization code for a token
	token, err := cfg.Exchange(context.Background(), result.code)
	if err != nil {
		return nil, fmt.Errorf("failed to exchange code for token: %w", err)
	}

	return token, nil
}

// openBrowser opens a URL in the default browser.
func openBrowser(url string) error {
	var cmd *exec.Cmd
	switch runtime.GOOS {
	case "darwin":
		cmd = exec.Command("open", url)
	case "linux":
		cmd = exec.Command("xdg-open", url)
	case "windows":
		cmd = exec.Command("rundll32", "url.dll,FileProtocolHandler", url)
	default:
		return fmt.Errorf("unsupported platform: %s", runtime.GOOS)
	}
	return cmd.Start()
}

// randomState generates a random hex string for the OAuth2 state parameter.
func randomState() (string, error) {
	b := make([]byte, 16)
	if _, err := rand.Read(b); err != nil {
		return "", err
	}
	return hex.EncodeToString(b), nil
}
