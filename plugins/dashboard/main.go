package main

import (
	"context"
	"embed"
	"log"

	"github.com/joeyhipolito/nanika-dashboard/internal"
	"github.com/wailsapp/wails/v2"
	"github.com/wailsapp/wails/v2/pkg/options"
	"github.com/wailsapp/wails/v2/pkg/options/assetserver"
	"github.com/wailsapp/wails/v2/pkg/options/mac"
)

//go:embed all:frontend/dist
var assets embed.FS

func main() {
	// Full-screen always-on overlay sized to the primary display.
	w, h := internal.PrimaryScreenSize()

	app := NewApp()

	err := wails.Run(&options.App{
		Title:  "Nanika",
		Width:  w,
		Height: h,

		Frameless:   true,
		AlwaysOnTop: true,

		// Fully transparent so only the React UI is visible.
		BackgroundColour: &options.RGBA{R: 0, G: 0, B: 0, A: 0},

		Mac: &mac.Options{
			TitleBar:             mac.TitleBarHidden(),
			WebviewIsTransparent: true,
			WindowIsTranslucent:  false,
		},

		AssetServer: &assetserver.Options{
			Assets: assets,
		},

		OnStartup: func(ctx context.Context) {
			app.startup(ctx)
			internal.SetContext(ctx)
			internal.SetupTray(ctx)
			internal.InitClickthrough()
			internal.RegisterHotkey()
		},

		Bind: []interface{}{
			app,
		},
	})
	if err != nil {
		log.Fatal(err)
	}
}
