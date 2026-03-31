package main

import (
	"context"
	"fmt"
	"time"
	"github.com/chromedp/chromedp"
)

func main() {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	allocCtx, allocCancel := chromedp.NewRemoteAllocator(ctx, "ws://localhost:9222")
	defer allocCancel()
	taskCtx, taskCancel := chromedp.NewContext(allocCtx)
	defer taskCancel()

	var result string
	err := chromedp.Run(taskCtx,
		chromedp.Navigate("https://www.google.com"),
		chromedp.Sleep(3*time.Second),
		chromedp.Evaluate(`JSON.stringify({title: document.title, htmlLen: document.body.innerHTML.length, links: document.querySelectorAll('a[href]').length, firstLinks: Array.from(document.querySelectorAll('a[href]')).slice(0,5).map(a => ({text: a.textContent.trim().slice(0,50), href: a.href.slice(0,80)}))})`, &result),
	)
	if err != nil {
		fmt.Println("Error:", err)
		return
	}
	fmt.Println(result)
}
