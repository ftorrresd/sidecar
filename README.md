# runk

Multi-language test project.

## Structure

- `python/` — CLI calculator (`python -m calc '2 + 3'`)
- `rust/` — String utilities library (levenshtein, hamming, capitalization, etc.)
- `cpp/` — Text file processor (word/line/char counts, find, freq)

## Build & Test

```sh
make all          # run all tests and build C++
make test-python  # run Python tests
make test-rust    # run Rust tests
make build-cpp    # build C++ project
```
