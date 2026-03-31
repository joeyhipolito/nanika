package api

import (
	"context"
	"fmt"
	"os"

	"google.golang.org/api/calendar/v3"
	"google.golang.org/api/drive/v3"
	"google.golang.org/api/gmail/v1"
	"google.golang.org/api/option"

	"github.com/joeyhipolito/nanika-gmail/internal/auth"
	"github.com/joeyhipolito/nanika-gmail/internal/config"
)

// Client wraps the Gmail, Calendar, and Drive API services for a single account.
type Client struct {
	svc      *gmail.Service
	calsvc   *calendar.Service
	drivesvc *drive.Service
	alias    string
}

// NewClient creates a Gmail and Calendar API client for the given account alias.
// Loads token from ~/.gmail/tokens/<alias>.json, creates oauth2 client with auto-refresh.
func NewClient(alias string, cfg *config.Config) (*Client, error) {
	tokenPath := config.TokenPath(alias)
	tok, err := auth.LoadToken(tokenPath)
	if err != nil {
		return nil, fmt.Errorf("load token for %s: %w", alias, err)
	}

	oauthCfg := auth.OAuthConfig(cfg.ClientID, cfg.ClientSecret)
	baseTS := oauthCfg.TokenSource(context.Background(), tok)
	ts := &auth.SavingTokenSource{
		Base: baseTS,
		Path: tokenPath,
	}

	svc, err := gmail.NewService(context.Background(),
		option.WithTokenSource(ts),
	)
	if err != nil {
		return nil, fmt.Errorf("create gmail service for %s: %w", alias, err)
	}

	calsvc, err := calendar.NewService(context.Background(),
		option.WithTokenSource(ts),
	)
	if err != nil {
		return nil, fmt.Errorf("create calendar service for %s: %w", alias, err)
	}

	drivesvc, err := drive.NewService(context.Background(),
		option.WithTokenSource(ts),
	)
	if err != nil {
		return nil, fmt.Errorf("create drive service for %s: %w", alias, err)
	}

	return &Client{svc: svc, calsvc: calsvc, drivesvc: drivesvc, alias: alias}, nil
}

// NewClientAll creates clients for all configured accounts.
// Skips accounts with invalid/missing tokens (logs warning to stderr).
func NewClientAll(cfg *config.Config) ([]*Client, error) {
	accounts, err := config.LoadAccounts()
	if err != nil {
		return nil, fmt.Errorf("load accounts: %w", err)
	}
	if len(accounts) == 0 {
		return nil, fmt.Errorf("no accounts configured. Run 'gmail configure <alias>' to add one")
	}

	var clients []*Client
	for _, acct := range accounts {
		c, err := NewClient(acct.Alias, cfg)
		if err != nil {
			fmt.Fprintf(os.Stderr, "warning: skipping account %q: %v\n", acct.Alias, err)
			continue
		}
		clients = append(clients, c)
	}

	if len(clients) == 0 {
		return nil, fmt.Errorf("no valid accounts found (checked %d)", len(accounts))
	}

	return clients, nil
}

// Alias returns the account alias.
func (c *Client) Alias() string {
	return c.alias
}

// Service returns the underlying Gmail service (for use by cmd layer if needed).
func (c *Client) Service() *gmail.Service {
	return c.svc
}
