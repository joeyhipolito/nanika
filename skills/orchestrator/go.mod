module github.com/joeyhipolito/orchestrator-cli

go 1.25.4

require (
	github.com/fsnotify/fsnotify v1.9.0
	github.com/joeyhipolito/nanika/shared/sdk v0.0.0-00010101000000-000000000000
	github.com/joeyhipolito/nen v0.0.0
	github.com/spf13/cobra v1.9.1
	github.com/spf13/pflag v1.0.6
	golang.org/x/text v0.35.0
	modernc.org/sqlite v1.48.0
)

replace (
	github.com/joeyhipolito/nanika/shared/sdk => ../../shared/sdk
	github.com/joeyhipolito/nen => ../../plugins/nen
)

require (
	github.com/dlclark/regexp2 v1.11.4 // indirect
	github.com/dop251/goja v0.0.0-20260311135729-065cd970411c // indirect
	github.com/dustin/go-humanize v1.0.1 // indirect
	github.com/go-sourcemap/sourcemap v2.1.3+incompatible // indirect
	github.com/google/pprof v0.0.0-20250317173921-a4b03ec1a45e // indirect
	github.com/google/uuid v1.6.0 // indirect
	github.com/inconshreveable/mousetrap v1.1.0 // indirect
	github.com/kr/text v0.2.0 // indirect
	github.com/mattn/go-isatty v0.0.20 // indirect
	github.com/ncruces/go-strftime v1.0.0 // indirect
	github.com/remyoudompheng/bigfft v0.0.0-20230129092748-24d4a6f8daec // indirect
	golang.org/x/sys v0.42.0 // indirect
	gopkg.in/yaml.v3 v3.0.1 // indirect
	modernc.org/libc v1.70.0 // indirect
	modernc.org/mathutil v1.7.1 // indirect
	modernc.org/memory v1.11.0 // indirect
)
