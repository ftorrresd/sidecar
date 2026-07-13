.PHONY: all clean test-python test-rust build-cpp

all: test-python test-rust build-cpp

test-python:
	cd python && python -m pytest tests/ -v

test-rust:
	cd rust && cargo test

build-cpp:
	cd cpp && mkdir -p build && cd build && cmake .. && make

clean:
	rm -rf cpp/build
	rm -rf rust/target
	rm -rf python/__pycache__ python/calc/__pycache__ python/tests/__pycache__
	rm -rf .pytest_cache
