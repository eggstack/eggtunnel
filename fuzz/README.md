# Decoder fuzz target

This directory is a separate Cargo workspace so `libfuzzer-sys` does not enter
the Eggtunnel production dependency graph. The target accepts arbitrary bytes
and calls the public, size-bounded `eggtunnel_proto::decode_frame` decoder.

With `cargo-fuzz` and a nightly Rust toolchain installed, run from the repo
root:

```sh
cargo +nightly fuzz build decode_frame
cargo +nightly fuzz run decode_frame fuzz/corpus/decode_frame -- -max_total_time=60 -max_len=1048590
cargo +nightly fuzz run decode_frame fuzz/corpus/decode_frame -- -runs=1000 -max_len=1048590
```

The checked-in corpus begins with valid minimal frames and malformed/truncated,
unsupported-version, unknown-message, and oversized-length frames. Coverage
corpus entries produced by fuzzing may be retained here. Inputs must remain
protocol bytes only: never add credentials, packet captures, or production
traffic. A bounded run is supporting evidence, not proof of exhaustive parser
safety.
