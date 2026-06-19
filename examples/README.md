# CSP example projects

Three tiny projects, each with one covered function (`add`) and one deliberately
uncovered function (`unused`), so coverage output has something to show.

The server **auto-collects**: if a project has no coverage artifact yet, it runs the
coverage tool itself (the commands below). Each project also ships a pre-generated
artifact so the demo is fast and deterministic even without the toolchains installed
— delete it to watch auto-generation kick in (`CSP_NO_AUTORUN` disables it).

Drive any of them with the debug client:

```sh
cargo run -p csp-cli -- --root examples/rust-sample
cargo run -p csp-cli -- --root examples/go-sample
cargo run -p csp-cli -- --root examples/ts-sample
```

## rust-sample  (`cargo-llvm-cov` adapter → `lcov.info`)

```sh
cd examples/rust-sample
cargo llvm-cov --lcov --output-path lcov.info
```

## go-sample  (`go` adapter → `coverage.out`)

```sh
cd examples/go-sample
go test -coverprofile=coverage.out ./...
```

## ts-sample  (`istanbul` adapter → `coverage/coverage-final.json`)

```sh
cd examples/ts-sample
npm install
npx vitest run --coverage --coverage.provider=istanbul
```

Expected result in every case: **50% line coverage** (1 of 2 lines), with `add`
covered and `unused` flagged uncovered.
