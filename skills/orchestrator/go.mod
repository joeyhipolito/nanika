module github.com/joeyhipolito/orchestrator-cli

go 1.25.4

require (
	github.com/fsnotify/fsnotify v1.9.0
	github.com/joeyhipolito/nen v0.0.0
	github.com/spf13/cobra v1.9.1
	github.com/spf13/pflag v1.0.6
	golang.org/x/text v0.35.0
	modernc.org/sqlite v1.45.0
)

replace github.com/joeyhipolito/nen => ../../plugins/nen

require (
	github.com/dustin/go-humanize v1.0.1 // indirect
	github.com/google/uuid v1.6.0 // indirect
	github.com/inconshreveable/mousetrap v1.1.0 // indirect
	github.com/mattn/go-isatty v0.0.20 // indirect
	github.com/ncruces/go-strftime v1.0.0 // indirect
	github.com/remyoudompheng/bigfft v0.0.0-20230129092748-24d4a6f8daec // indirect
	golang.org/x/exp v0.0.0-20251023183803-a4bb9ffd2546 // indirect
	golang.org/x/sync v0.20.0 // indirect
	golang.org/x/sys v0.41.0 // indirect
	modernc.org/libc v1.67.6 // indirect
	modernc.org/mathutil v1.7.1 // indirect
	modernc.org/memory v1.11.0 // indirect
)
