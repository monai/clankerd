// Package lock is an advisory exclusive file lock. The kernel drops it when the holder exits or crashes.
package lock

import (
	"errors"
	"os"
	"syscall"
)

type File struct{ f *os.File }

func open(path string, flags int) (*File, bool, error) {
	f, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return nil, false, err
	}
	if err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX|flags); err != nil {
		f.Close()
		if errors.Is(err, syscall.EWOULDBLOCK) {
			return nil, false, nil
		}
		return nil, false, err
	}
	return &File{f}, true, nil
}

// Acquire blocks until it holds the lock.
func Acquire(path string) (*File, error) {
	l, _, err := open(path, 0)
	return l, err
}

// TryAcquire reports false instead of waiting when another process holds the lock.
func TryAcquire(path string) (*File, bool, error) { return open(path, syscall.LOCK_NB) }

// WritePID replaces the file's contents with pid.
func (l *File) WritePID(pid string) error {
	if err := l.f.Truncate(0); err != nil {
		return err
	}
	_, err := l.f.WriteAt([]byte(pid+"\n"), 0)
	return err
}

func (l *File) Release() { l.f.Close() }
