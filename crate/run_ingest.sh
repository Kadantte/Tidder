#!/bin/bash

set -e
shopt -s extglob

for URL in $(< ~/tidder/crate/ingest/todo.txt); do
    ~/.cargo/bin/cargo build --bin ingest --release
    cd /mnt/permanent/archives
    rm *
    wget $URL
    7z x *
    RUST_LOG="info" ~/tidder/crate/target/release/ingest $@ !(*.*)
    tail -n +2i ~/tidder/crate/ingest/todo.txt | sponge ~/tidder/crate/ingest/todo.txt
    rm *
done
