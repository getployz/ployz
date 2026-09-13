package main

import (
	"encoding/base64"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"testing"
)

func TestExceptionCanMatchDescendant(t *testing.T) {
	for _, test := range []struct {
		pattern, directory string
		want               bool
	}{
		{"src/*.rs", "node_modules", false},
		{"src/foo*.rs", "src/bar", false},
		{"src/*.rs", "src", true},
		{"src/foo*.rs", "src/foo", true},
		{"**/keep", "node_modules", true},
		{"cache/keep", "cache", true},
		{"cache/keep", "cache-other", false},
		{"cache", "cache", false},
		{`src/\[literal\]/keep`, "src/[literal]", true},
	} {
		t.Run(test.pattern+":"+test.directory, func(t *testing.T) {
			if got := exceptionCanMatchDescendant(test.pattern, test.directory); got != test.want {
				t.Fatalf("got %v, want %v", got, test.want)
			}
		})
	}
}

func TestRailpackContextMergesExclusionsAndKeepsControlFiles(t *testing.T) {
	root := t.TempDir()
	for name, content := range map[string]string{
		".dockerignore": "*.txt\n.dockerignore\nrailpack.json\n",
		"railpack.json": `{"exclude":["!keep.txt","drop.log"]}`,
		"keep.txt":      "included by config negation", "drop.txt": "private", "drop.log": "private",
	} {
		if err := os.WriteFile(filepath.Join(root, name), []byte(content), 0600); err != nil {
			t.Fatal(err)
		}
	}
	// Use the actual helper request wire format so an ignored new option fails.
	var request contextRequest
	if err := json.Unmarshal([]byte(fmt.Sprintf(`{"path":%q,"railpack_config":"railpack.json"}`, root)), &request); err != nil {
		t.Fatal(err)
	}
	result, err := contextFiles(request)
	if err != nil {
		t.Fatal(err)
	}
	included := map[string]bool{}
	for _, path := range result.Paths {
		decoded, err := base64.StdEncoding.DecodeString(path)
		if err != nil {
			t.Fatal(err)
		}
		included[string(decoded)] = true
	}
	for name, want := range map[string]bool{"keep.txt": true, "drop.txt": false, "drop.log": false, "railpack.json": true, ".dockerignore": true} {
		if included[name] != want {
			t.Errorf("%s: included=%v, want %v", name, included[name], want)
		}
	}
}

func TestRailpackConfigLinkKeepsItsExcludedTarget(t *testing.T) {
	root := t.TempDir()
	if err := os.WriteFile(filepath.Join(root, "real.json"), []byte(`{"exclude":["real.json","railpack.json"]}`), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink("real.json", filepath.Join(root, "railpack.json")); err != nil {
		t.Fatal(err)
	}
	result, err := contextFiles(contextRequest{Path: root, RailpackConfig: "./railpack.json"})
	if err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"real.json", "railpack.json"} {
		found := false
		for _, path := range result.Paths {
			if path == base64.StdEncoding.EncodeToString([]byte(name)) {
				found = true
			}
		}
		if !found {
			t.Errorf("missing required configuration entry %s", name)
		}
	}
}
