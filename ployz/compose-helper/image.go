package main

import (
	"bytes"
	"context"
	"fmt"
	"os/exec"
	"strconv"
	"strings"
	"text/template"
	"time"
	_ "time/tzdata"

	"github.com/distribution/reference"
)

type imageState struct {
	sha    string
	commit time.Time
	dirty  bool
}

func inspectImages(ctx context.Context, directory string) (imageState, error) {
	git := func(args ...string) (string, error) {
		cmd := exec.CommandContext(ctx, "git", args...)
		cmd.Dir = directory
		data, err := cmd.Output()
		return strings.TrimSpace(string(data)), err
	}
	sha, err := git("rev-parse", "--verify", "HEAD")
	if err != nil {
		return imageState{}, nil
	}
	timestamp, err := git("log", "-1", "--format=%ct")
	if err != nil {
		return imageState{}, err
	}
	seconds, err := strconv.ParseInt(timestamp, 10, 64)
	if err != nil {
		return imageState{}, err
	}
	status, err := git("status", "--porcelain")
	if err != nil {
		return imageState{}, err
	}
	return imageState{sha: sha, commit: time.Unix(seconds, 0).UTC(), dirty: status != ""}, nil
}
func (i imageState) image(project, service, image string, build bool) (string, error) {
	short := func(length ...int) string {
		if len(length) > 0 && length[0] > 0 && length[0] < len(i.sha) {
			return i.sha[:length[0]]
		}
		return i.sha
	}
	format := func(t time.Time, layout string, zones ...string) (string, error) {
		zone := time.UTC
		if len(zones) > 0 {
			var err error
			zone, err = time.LoadLocation(zones[0])
			if err != nil {
				return "", err
			}
		}
		return t.In(zone).Format(layout), nil
	}
	tag := time.Now().UTC().Format("2006-01-02-150405")
	if !i.commit.IsZero() {
		tag = i.commit.Format("2006-01-02-150405") + "." + short(7)
		if i.dirty {
			tag += ".dirty"
		}
	}
	if image == "" && build {
		image = project + "/" + service + ":{{.Tag}}"
	}
	tmpl, err := template.New("image").Option("missingkey=error").Funcs(template.FuncMap{
		"gitsha": short,
		"gitdate": func(layout string, zones ...string) (string, error) {
			if i.commit.IsZero() {
				return "", nil
			}
			return format(i.commit, layout, zones...)
		},
		"date": func(layout string, zones ...string) (string, error) { return format(time.Now(), layout, zones...) },
	}).Parse(image)
	if err != nil {
		return "", err
	}
	var output bytes.Buffer
	err = tmpl.Execute(&output, object{"Project": project, "Service": service, "Tag": tag, "Git": object{"IsRepo": !i.commit.IsZero(), "IsDirty": i.dirty, "SHA": i.sha}})
	if err != nil {
		return "", err
	}
	image = output.String()
	if image == "" {
		return "", nil
	}
	if !build {
		last := image[strings.LastIndex(image, "/")+1:]
		if !strings.ContainsAny(last, ":@") {
			image += ":latest"
		}
		return image, nil
	}
	parsed, err := reference.ParseNormalizedNamed(image)
	if err != nil {
		return "", fmt.Errorf("parse image reference '%s': %w", image, err)
	}
	_, tagged := parsed.(reference.NamedTagged)
	_, digested := parsed.(reference.Digested)
	if !tagged && !digested {
		image += ":" + tag
	}
	return image, nil
}
