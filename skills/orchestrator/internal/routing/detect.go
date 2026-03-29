package routing

import (
	"os"
	"path/filepath"
	"strings"
)

// DetectProfile scans the root of repoPath for well-known files to infer a
// TargetProfile. Only root-level files are inspected so detection completes
// well within the 500 ms budget even on large repos.
//
// The returned profile's TargetID is always empty — callers must set it.
// Fields that cannot be detected are left as zero values so they can be merged
// with a profile retrieved from the routing DB: non-empty DB values win.
//
// DetectProfile never returns an error; inaccessible paths yield an empty
// profile rather than a failure, satisfying the graceful-fallback requirement.
func DetectProfile(repoPath string) TargetProfile {
	entries, err := os.ReadDir(repoPath)
	if err != nil {
		return TargetProfile{}
	}

	// Collect the names of root-level files and directories for fast lookup.
	files := make(map[string]bool, len(entries))
	var dirs []string
	for _, e := range entries {
		files[e.Name()] = true
		if e.IsDir() && !strings.HasPrefix(e.Name(), ".") {
			dirs = append(dirs, e.Name())
		}
	}

	var p TargetProfile
	p.KeyDirectories = detectKeyDirs(dirs)

	switch {
	case files["go.mod"]:
		p.Language = "go"
		p.Runtime = "go"
		p.TestCommand = "go test ./..."
		p.BuildCommand = detectGoBuild(repoPath, files)
		p.Framework = detectGoFramework(repoPath)

	case files["Cargo.toml"]:
		p.Language = "rust"
		p.Runtime = "cargo"
		p.TestCommand = "cargo test"
		p.BuildCommand = "cargo build --release"
		p.Framework = detectRustFramework(repoPath)

	case files["package.json"]:
		p.Language = "typescript"
		p.Runtime = "node"
		p.TestCommand, p.BuildCommand, p.Framework = detectNodeProfile(repoPath)
		if p.Language == "typescript" && !files["tsconfig.json"] {
			p.Language = "javascript"
		}

	case files["pyproject.toml"] || files["setup.py"] || files["requirements.txt"]:
		p.Language = "python"
		p.Runtime = "python"
		p.TestCommand = detectPythonTest(repoPath, files)
		p.BuildCommand = ""
		p.Framework = detectPythonFramework(repoPath, files)

	case files["pom.xml"]:
		p.Language = "java"
		p.Runtime = "maven"
		p.TestCommand = "mvn test"
		p.BuildCommand = "mvn package"

	case files["build.gradle"] || files["build.gradle.kts"]:
		p.Language = "java"
		p.Runtime = "gradle"
		p.TestCommand = "./gradlew test"
		p.BuildCommand = "./gradlew build"
	}

	// Makefile overrides: if a Makefile exists, check for test/build targets and
	// prefer them so repo-specific conventions win over generic defaults.
	if files["Makefile"] {
		if t := detectMakeTarget(repoPath, "test"); t != "" && p.TestCommand == "" {
			p.TestCommand = t
		}
		if b := detectMakeTarget(repoPath, "build"); b != "" && p.BuildCommand == "" {
			p.BuildCommand = b
		}
	}

	return p
}

// MergeDetected fills zero-value fields in dst with values from src (detected).
// Fields already set in dst (from the routing DB) are never overwritten.
func MergeDetected(dst, src TargetProfile) TargetProfile {
	if dst.Language == "" {
		dst.Language = src.Language
	}
	if dst.Runtime == "" {
		dst.Runtime = src.Runtime
	}
	if dst.TestCommand == "" {
		dst.TestCommand = src.TestCommand
	}
	if dst.BuildCommand == "" {
		dst.BuildCommand = src.BuildCommand
	}
	if dst.Framework == "" {
		dst.Framework = src.Framework
	}
	if len(dst.KeyDirectories) == 0 {
		dst.KeyDirectories = src.KeyDirectories
	}
	return dst
}

// detectKeyDirs returns a filtered list of notable top-level directories.
var notableDirs = map[string]bool{
	"cmd": true, "internal": true, "pkg": true, "src": true, "lib": true,
	"api": true, "server": true, "client": true, "web": true, "app": true,
	"test": true, "tests": true, "e2e": true, "docs": true,
}

func detectKeyDirs(dirs []string) []string {
	var out []string
	for _, d := range dirs {
		if notableDirs[d] {
			out = append(out, d)
		}
	}
	return out
}

// detectGoBuild returns the most appropriate build command for a Go repo.
// Prefers "make build" when a Makefile with a build target is present.
func detectGoBuild(repoPath string, files map[string]bool) string {
	if files["Makefile"] {
		if t := detectMakeTarget(repoPath, "build"); t != "" {
			return t
		}
	}
	return "go build ./..."
}

// detectGoFramework scans go.mod for a known framework import path.
func detectGoFramework(repoPath string) string {
	data, err := os.ReadFile(filepath.Join(repoPath, "go.mod"))
	if err != nil {
		return ""
	}
	content := string(data)
	switch {
	case strings.Contains(content, "github.com/spf13/cobra"):
		return "cobra"
	case strings.Contains(content, "github.com/gin-gonic/gin"):
		return "gin"
	case strings.Contains(content, "github.com/labstack/echo"):
		return "echo"
	case strings.Contains(content, "google.golang.org/grpc"):
		return "grpc"
	case strings.Contains(content, "github.com/gorilla/mux"):
		return "gorilla/mux"
	default:
		return ""
	}
}

// detectRustFramework scans Cargo.toml for a known framework dependency.
func detectRustFramework(repoPath string) string {
	data, err := os.ReadFile(filepath.Join(repoPath, "Cargo.toml"))
	if err != nil {
		return ""
	}
	content := string(data)
	switch {
	case strings.Contains(content, "axum"):
		return "axum"
	case strings.Contains(content, "actix-web"):
		return "actix-web"
	case strings.Contains(content, "tokio"):
		return "tokio"
	case strings.Contains(content, "warp"):
		return "warp"
	default:
		return ""
	}
}

// detectNodeProfile reads package.json to extract test/build scripts and framework.
func detectNodeProfile(repoPath string) (testCmd, buildCmd, framework string) {
	data, err := os.ReadFile(filepath.Join(repoPath, "package.json"))
	if err != nil {
		return "npm test", "npm run build", ""
	}
	content := string(data)

	// Script detection — look for common script key patterns.
	if strings.Contains(content, `"test"`) {
		testCmd = "npm test"
	}
	if strings.Contains(content, `"build"`) {
		buildCmd = "npm run build"
	}

	// Framework detection by dependency name.
	switch {
	case strings.Contains(content, `"next"`):
		framework = "next.js"
	case strings.Contains(content, `"react"`):
		framework = "react"
	case strings.Contains(content, `"vue"`):
		framework = "vue"
	case strings.Contains(content, `"svelte"`):
		framework = "svelte"
	case strings.Contains(content, `"express"`):
		framework = "express"
	case strings.Contains(content, `"fastify"`):
		framework = "fastify"
	}

	if testCmd == "" {
		testCmd = "npm test"
	}
	return testCmd, buildCmd, framework
}

// detectPythonTest returns the test command for a Python repo.
func detectPythonTest(repoPath string, files map[string]bool) string {
	if files["pytest.ini"] || files["pyproject.toml"] {
		return "pytest"
	}
	return "python -m pytest"
}

// detectPythonFramework scans requirements files for known frameworks.
func detectPythonFramework(repoPath string, files map[string]bool) string {
	candidates := []string{"requirements.txt", "requirements-dev.txt", "pyproject.toml"}
	for _, f := range candidates {
		if !files[f] {
			continue
		}
		data, err := os.ReadFile(filepath.Join(repoPath, f))
		if err != nil {
			continue
		}
		content := strings.ToLower(string(data))
		switch {
		case strings.Contains(content, "fastapi"):
			return "fastapi"
		case strings.Contains(content, "django"):
			return "django"
		case strings.Contains(content, "flask"):
			return "flask"
		case strings.Contains(content, "starlette"):
			return "starlette"
		}
	}
	return ""
}

// detectMakeTarget returns "make <target>" if the Makefile defines that target,
// or "" if not found or unreadable.
func detectMakeTarget(repoPath, target string) string {
	data, err := os.ReadFile(filepath.Join(repoPath, "Makefile"))
	if err != nil {
		return ""
	}
	// A Makefile target starts at column 0 followed by a colon.
	needle := target + ":"
	for _, line := range strings.Split(string(data), "\n") {
		if strings.HasPrefix(line, needle) {
			return "make " + target
		}
	}
	return ""
}
