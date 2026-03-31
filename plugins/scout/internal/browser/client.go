// Package browser provides a CDP (Chrome DevTools Protocol) client that
// connects to a running Chrome/Chromium instance on localhost:9222.
// It is used by browser-based gatherers in the gather package to scrape
// personalized feeds that require an active browser session.
package browser

import (
	"context"
	"fmt"
	"time"

	"github.com/chromedp/chromedp"
)

// DefaultCDPURL is the standard Chrome remote debugging WebSocket endpoint.
const DefaultCDPURL = "ws://localhost:9222"

// Client manages a remote allocator connection to a running Chrome instance.
// Create with New(); call Close() when done. Safe for sequential use.
type Client struct {
	allocCtx    context.Context
	allocCancel context.CancelFunc
}

// New connects to Chrome at cdpURL (use DefaultCDPURL for the standard local setup).
// Returns an error if Chrome is unreachable or not running with remote debugging enabled.
// Chrome must be launched with: --remote-debugging-port=9222
func New(ctx context.Context, cdpURL string) (*Client, error) {
	allocCtx, allocCancel := chromedp.NewRemoteAllocator(ctx, cdpURL)

	// Verify the connection by creating and immediately closing a test tab.
	testCtx, testCancel := chromedp.NewContext(allocCtx)
	if err := chromedp.Run(testCtx); err != nil {
		testCancel()
		allocCancel()
		return nil, fmt.Errorf("connecting to Chrome at %s: %w (start Chrome with --remote-debugging-port=9222)", cdpURL, err)
	}
	testCancel()

	return &Client{allocCtx: allocCtx, allocCancel: allocCancel}, nil
}

// Close releases the CDP allocator and all associated tab contexts.
func (c *Client) Close() {
	c.allocCancel()
}

// PageOptions configures browser navigation behaviour.
type PageOptions struct {
	// WaitSelector is a CSS selector to wait for before evaluating JS.
	// If empty, defaults to "body".
	WaitSelector string
	// Scrolls is the number of times to scroll the page to trigger lazy loading.
	Scrolls int
	// IdleTimeout is the maximum time to wait for page activity to settle.
	// Defaults to 3 seconds if zero.
	IdleTimeout time.Duration
}

// Eval navigates a new tab to pageURL, waits for content, and evaluates jsExpr.
// The result of jsExpr is written into result (must be a pointer to a string).
// The tab is closed after the call returns.
func (c *Client) Eval(ctx context.Context, pageURL, jsExpr string, result *string, opts PageOptions) error {
	tabCtx, tabCancel := chromedp.NewContext(c.allocCtx)
	defer tabCancel()

	waitSel := opts.WaitSelector
	if waitSel == "" {
		waitSel = "body"
	}
	idleTimeout := opts.IdleTimeout
	if idleTimeout == 0 {
		idleTimeout = 3 * time.Second
	}

	actions := []chromedp.Action{
		chromedp.Navigate(pageURL),
		chromedp.WaitVisible(waitSel, chromedp.ByQuery),
		chromedp.Sleep(idleTimeout),
	}

	for i := 0; i < opts.Scrolls; i++ {
		actions = append(actions,
			chromedp.Evaluate(`window.scrollBy(0, window.innerHeight)`, nil),
			chromedp.Sleep(800*time.Millisecond),
		)
	}

	actions = append(actions, chromedp.Evaluate(jsExpr, result))

	if err := chromedp.Run(tabCtx, actions...); err != nil {
		return fmt.Errorf("page %s: %w", pageURL, err)
	}
	return nil
}
