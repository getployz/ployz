package main

import (
	"encoding/base64"
	"io/fs"
	"os"
	"path/filepath"
	"strings"

	"github.com/moby/patternmatcher"
	"github.com/moby/patternmatcher/ignorefile"
)

type contextRequest struct {
	Path       string `json:"path"`
	Dockerfile string `json:"dockerfile"`
}

type contextSelection struct {
	Paths  []string `json:"paths"`
	Ignore []byte   `json:"ignore"`
}

// Use Docker's parser and matcher; gitignore semantics are different.
func contextFiles(req contextRequest) (contextSelection, error) {
	var result contextSelection
	var content []byte
	var err error
	rootIgnore := req.Dockerfile == ""
	if req.Dockerfile != "" {
		content, err = os.ReadFile(req.Dockerfile + ".dockerignore")
	}
	if req.Dockerfile == "" || os.IsNotExist(err) {
		rootIgnore = true
		content, err = os.ReadFile(filepath.Join(req.Path, ".dockerignore"))
	}
	if err != nil && !os.IsNotExist(err) {
		return result, err
	}
	patterns, err := ignorefile.ReadAll(strings.NewReader(string(content)))
	if err != nil {
		return result, err
	}
	matcher, err := patternmatcher.New(patterns)
	if err != nil {
		return result, err
	}
	included := map[string]bool{".": true}
	err = filepath.WalkDir(req.Path, func(path string, entry fs.DirEntry, walkErr error) error {
		relative, err := filepath.Rel(req.Path, path)
		if err != nil {
			return err
		}
		if relative == "." {
			return walkErr
		}
		ignored, err := matcher.MatchesOrParentMatches(relative)
		if err != nil {
			return err
		}
		// Keep the active root ignore file for Docker, even if it excludes itself.
		if rootIgnore && relative == ".dockerignore" {
			ignored = false
		}
		if walkErr != nil {
			// BuildKit ignores permission errors in excluded directories, including
			// those visited to look for negated patterns.
			if ignored && os.IsPermission(walkErr) {
				return nil
			}
			return walkErr
		}
		if ignored {
			if !entry.IsDir() {
				return nil
			}
			// Only prune when no exception can select a descendant (BuildKit semantics).
			for _, pattern := range matcher.Patterns() {
				if pattern.Exclusion() && exceptionCanMatchDescendant(pattern.String(), relative) {
					return nil
				}
			}
			return filepath.SkipDir
		}
		for name := relative; name != "."; name = filepath.Dir(name) {
			included[name] = true
		}
		return nil
	})
	if err != nil {
		return result, err
	}
	result.Ignore = content
	result.Paths = make([]string, 0, len(included))
	for name := range included {
		// JSON strings cannot round-trip arbitrary Unix filenames.
		result.Paths = append(result.Paths, base64.StdEncoding.EncodeToString([]byte(name)))
	}
	return result, nil
}

// A wildcard cannot match outside the literal prefix preceding it. Keep the
// remaining cases conservative so ** and escaped patterns retain descendants.
func exceptionCanMatchDescendant(pattern, directory string) bool {
	directory += string(filepath.Separator)
	if wildcard := strings.IndexAny(pattern, "*[]?^\\"); wildcard >= 0 {
		prefix := pattern[:wildcard]
		return strings.HasPrefix(directory, prefix) || strings.HasPrefix(prefix, directory)
	}
	return strings.HasPrefix(pattern, directory)
}
