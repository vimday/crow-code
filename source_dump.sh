#!/bin/bash
echo "# crow-code source dump" > source_dump.txt
find crates -name "*.rs" -o -name "Cargo.toml" | while read -r file; do
    echo "======================================" >> source_dump.txt
    echo "File: $file" >> source_dump.txt
    echo "======================================" >> source_dump.txt
    cat "$file" >> source_dump.txt
    echo "" >> source_dump.txt
done
echo "Dumped to source_dump.txt"
