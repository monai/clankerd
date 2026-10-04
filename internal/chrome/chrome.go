// Package chrome starts, stops and finds the host Chrome of a lease.
package chrome

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"syscall"
	"time"
)

var candidates = []string{
	"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
	"google-chrome", "google-chrome-stable", "chromium", "chromium-browser",
}

// Find returns the configured Chrome binary, or the first one found on this machine.
func Find(configured string) (string, error) {
	if configured != "" {
		p, err := exec.LookPath(configured)
		if err != nil {
			return "", fmt.Errorf("chrome binary %q not found (check CLANKERD_CHROME_BIN)", configured)
		}
		return p, nil
	}
	for _, c := range candidates {
		if p, err := exec.LookPath(c); err == nil {
			return p, nil
		}
	}
	return "", fmt.Errorf("no Chrome found on the host; set CLANKERD_CHROME_BIN")
}

// Start launches Chrome with its own profile and the debug port, in its own session so it outlives
// the daemon. It returns the process id. Nothing is navigated.
func Start(bin, profile string, debugPort int) (int, error) {
	if err := os.MkdirAll(profile, 0o700); err != nil {
		return 0, err
	}
	cmd := exec.Command(bin,
		"--user-data-dir="+profile,
		fmt.Sprintf("--remote-debugging-port=%d", debugPort),
		"--no-first-run", "--no-default-browser-check")
	cmd.SysProcAttr = &syscall.SysProcAttr{Setsid: true}
	devnull, err := os.OpenFile(os.DevNull, os.O_RDWR, 0)
	if err != nil {
		return 0, err
	}
	defer devnull.Close()
	cmd.Stdin, cmd.Stdout, cmd.Stderr = devnull, devnull, devnull
	cmd.Dir = filepath.Dir(profile)
	if err := cmd.Start(); err != nil {
		return 0, err
	}
	go cmd.Wait() // reap it when it exits
	return cmd.Process.Pid, nil
}

// Alive reports whether a process with this id exists.
func Alive(pid int) bool {
	return pid > 0 && syscall.Kill(pid, 0) == nil
}

// Stop asks Chrome to exit and kills it if it has not within a few seconds.
func Stop(pid int) {
	if !Alive(pid) {
		return
	}
	syscall.Kill(pid, syscall.SIGTERM)
	for i := 0; i < 50; i++ {
		if !Alive(pid) {
			return
		}
		time.Sleep(100 * time.Millisecond)
	}
	syscall.Kill(pid, syscall.SIGKILL)
}
