.DEFAULT_GOAL := build
.PHONY: build rust rust-build rust-sign test rust-test test-linux test-native lint rust-lint coverage rust-coverage coverage-setup rust-coverage-setup clean

# mise install supplies the pinned toolchain; all build phases run in one context.
build:
	python3 scripts/rust-build.py build

rust rust-build: build

rust-sign:
	python3 scripts/rust-build.py sign

test:
	python3 -m unittest discover -s scripts/tests -p 'test_rust_*.py'
	python3 -m unittest discover -s scripts/tests -p 'test_dependency_build.py'
	python3 scripts/rust-test.py test

rust-test: test

test-linux:
	python3 scripts/rust-test.py test-linux

test-native:
	python3 scripts/rust-test.py test-native

lint:
	python3 scripts/rust-test.py lint

rust-lint: lint

coverage: coverage-setup
	python3 scripts/rust-test.py coverage

rust-coverage: coverage

coverage-setup:
	python3 scripts/rust-test.py coverage-setup

rust-coverage-setup: coverage-setup

clean:
	rm -rf build target
