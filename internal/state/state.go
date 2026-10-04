// Package state persists the daemon's leases as one file, written atomically.
package state

import (
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
)

type Lease struct {
	Name  string   `json:"name"`
	Slot  int      `json:"slot"`
	Hosts []string `json:"hosts"`
	// ChromePID is the recorded Chrome process, 0 when none.
	ChromePID int `json:"chrome_pid,omitempty"`
}

type State struct {
	Leases []Lease `json:"leases"`
}

func Load(path string) (*State, error) {
	b, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return &State{}, nil
	}
	if err != nil {
		return nil, err
	}
	var s State
	if err := json.Unmarshal(b, &s); err != nil {
		return nil, err
	}
	return &s, nil
}

func Save(path string, s *State) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	b, err := json.MarshalIndent(s, "", "  ")
	if err != nil {
		return err
	}
	tmp, err := os.CreateTemp(filepath.Dir(path), ".state-*")
	if err != nil {
		return err
	}
	defer os.Remove(tmp.Name())
	if _, err := tmp.Write(b); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Sync(); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	return os.Rename(tmp.Name(), path)
}
