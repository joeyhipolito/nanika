package api

import (
	"fmt"
	"io"
	"os"

	"google.golang.org/api/drive/v3"
)

const driveFileFields = "files(id,name,mimeType,size,modifiedTime,webViewLink,owners/emailAddress)"

// ListDriveFiles returns up to limit recently modified files from Google Drive,
// ordered by modification time descending.
func (c *Client) ListDriveFiles(limit int) ([]DriveFile, error) {
	if limit <= 0 {
		limit = 10
	}
	var files []DriveFile
	err := withRetry(func() error {
		resp, err := c.drivesvc.Files.List().
			Fields(driveFileFields).
			OrderBy("modifiedTime desc").
			PageSize(int64(limit)).
			Do()
		if err != nil {
			return err
		}
		files = make([]DriveFile, 0, len(resp.Files))
		for _, f := range resp.Files {
			files = append(files, toDriveFile(c.alias, f))
		}
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("listing drive files for %s: %w", c.alias, err)
	}
	return files, nil
}

// SearchDriveFiles searches Drive for files matching query by name or full-text content,
// returning up to limit results ordered by modification time descending.
func (c *Client) SearchDriveFiles(query string, limit int) ([]DriveFile, error) {
	if query == "" {
		return nil, fmt.Errorf("query is required")
	}
	if limit <= 0 {
		limit = 10
	}
	q := fmt.Sprintf("(name contains %q or fullText contains %q) and trashed = false", query, query)
	var files []DriveFile
	err := withRetry(func() error {
		resp, err := c.drivesvc.Files.List().
			Fields(driveFileFields).
			Q(q).
			OrderBy("modifiedTime desc").
			PageSize(int64(limit)).
			Do()
		if err != nil {
			return err
		}
		files = make([]DriveFile, 0, len(resp.Files))
		for _, f := range resp.Files {
			files = append(files, toDriveFile(c.alias, f))
		}
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("searching drive for %q (%s): %w", query, c.alias, err)
	}
	return files, nil
}

// DownloadDriveFile downloads the file with fileID to outputPath.
// If outputPath is empty, the file is saved in the current directory using the file's name.
// Google-native files (Docs, Sheets, Slides) are exported to Office formats (.docx, .xlsx, .pptx).
// Returns the path where the file was saved.
func (c *Client) DownloadDriveFile(fileID, outputPath string) (string, error) {
	if fileID == "" {
		return "", fmt.Errorf("file ID is required")
	}

	// Fetch metadata to resolve name and MIME type.
	var meta *drive.File
	err := withRetry(func() error {
		var err error
		meta, err = c.drivesvc.Files.Get(fileID).Fields("id,name,mimeType").Do()
		return err
	})
	if err != nil {
		return "", fmt.Errorf("getting metadata for file %s: %w", fileID, err)
	}

	exportMIME, ext := googleExportFormat(meta.MimeType)
	if outputPath == "" {
		outputPath = meta.Name + ext
	}

	// Retry only the network call — local file I/O errors should fail fast.
	var body io.ReadCloser
	err = withRetry(func() error {
		if exportMIME != "" {
			r, rerr := c.drivesvc.Files.Export(fileID, exportMIME).Download()
			if rerr != nil {
				return rerr
			}
			body = r.Body
		} else {
			r, rerr := c.drivesvc.Files.Get(fileID).Download()
			if rerr != nil {
				return rerr
			}
			body = r.Body
		}
		return nil
	})
	if err != nil {
		return "", fmt.Errorf("downloading file %s: %w", fileID, err)
	}
	defer body.Close()

	f, err := os.OpenFile(outputPath, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0600)
	if err != nil {
		return "", fmt.Errorf("creating output file %q: %w", outputPath, err)
	}
	defer f.Close()

	if _, err = io.Copy(f, body); err != nil {
		return "", fmt.Errorf("writing %q: %w", outputPath, err)
	}
	return outputPath, nil
}

// googleExportFormat returns the export MIME type and file extension for Google-native formats.
// Returns empty strings for non-Google-native formats (regular files are downloaded directly).
func googleExportFormat(mimeType string) (exportMIME, ext string) {
	switch mimeType {
	case "application/vnd.google-apps.document":
		return "application/vnd.openxmlformats-officedocument.wordprocessingml.document", ".docx"
	case "application/vnd.google-apps.spreadsheet":
		return "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", ".xlsx"
	case "application/vnd.google-apps.presentation":
		return "application/vnd.openxmlformats-officedocument.presentationml.presentation", ".pptx"
	default:
		return "", ""
	}
}

// toDriveFile converts a *drive.File to our DriveFile type.
func toDriveFile(alias string, f *drive.File) DriveFile {
	df := DriveFile{
		ID:           f.Id,
		Account:      alias,
		Name:         f.Name,
		MimeType:     f.MimeType,
		Size:         f.Size,
		ModifiedTime: f.ModifiedTime,
		WebViewLink:  f.WebViewLink,
	}
	for _, o := range f.Owners {
		df.Owners = append(df.Owners, o.EmailAddress)
	}
	return df
}
