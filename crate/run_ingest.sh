#!/bin/bash

set -e

~/.cargo/bin/cargo build --bin ingest --release

for URL in $(< ingest/todo.txt); do
    target/release/ingest $@ $URL
    tail -n +2 ingest/todo.txt | sponge ingest/todo.txt
done
