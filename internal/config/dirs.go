package config

import (
	"crypto/sha256"
	"fmt"
	"os"
	"path/filepath"
	"slices"

	"github.com/adrg/xdg"
)

// maxSocketPath keeps unix socket paths under the macOS limit of about 104 bytes.
const maxSocketPath = 100

type Dirs struct {
	State   string
	Runtime string
	VM      string
}

func (d Dirs) Socket() string    { return filepath.Join(d.Runtime, d.VM+".sock") }
func (d Dirs) PIDFile() string   { return filepath.Join(d.Runtime, d.VM+".pid") }
func (d Dirs) StateFile() string { return filepath.Join(d.State, "state.json") }
func (d Dirs) LogFile() string   { return filepath.Join(d.State, "clankerd.log") }
func (d Dirs) Profile(name string) string {
	return filepath.Join(d.State, "profiles", name)
}

type layout struct {
	root string
}

func locate(home, cwd string) (layout, []string) {
	if home != "" {
		return layout{root: home}, []string{filepath.Join(home, "config.toml")}
	}
	var files []string
	for _, d := range slices.Backward(xdg.ConfigDirs) {
		files = append(files, filepath.Join(d, "clankerd", "config.toml"))
	}
	files = append(files, filepath.Join(xdg.ConfigHome, "clankerd", "config.toml"))
	if p := findProject(cwd); p != "" {
		files = append(files, filepath.Join(p, "config.toml"))
		return layout{root: p}, files
	}
	return layout{}, files
}

func findProject(dir string) string {
	for {
		p := filepath.Join(dir, ".clankerd")
		if fi, err := os.Stat(p); err == nil && fi.IsDir() {
			return p
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			return ""
		}
		dir = parent
	}
}

func resolveDirs(b layout, vm string) (Dirs, error) {
	d := Dirs{VM: vm}
	if b.root != "" {
		abs, err := filepath.Abs(b.root)
		if err != nil {
			return d, err
		}
		d.State = filepath.Join(abs, "state", vm)
		d.Runtime = filepath.Join(abs, "run")
	} else {
		d.State = filepath.Join(xdg.StateHome, "clankerd", vm)
		d.Runtime = filepath.Join(xdg.RuntimeDir, "clankerd")
	}
	if len(d.Socket()) > maxSocketPath {
		sum := sha256.Sum256([]byte(d.Runtime))
		d.Runtime = filepath.Join(os.TempDir(), fmt.Sprintf("clankerd-%d-%x", os.Getuid(), sum[:4]))
		if len(d.Socket()) > maxSocketPath {
			return d, fmt.Errorf("control socket path %q is longer than %d bytes", d.Socket(), maxSocketPath)
		}
	}
	return d, nil
}
