#!/usr/bin/env bash

# Check if a directory was provided, otherwise default to the current directory
TARGET_DIR="${1:-.}"

if [ ! -d "$TARGET_DIR" ]; then
    echo "Error: Directory '$TARGET_DIR' does not exist."
    exit 1
fi

echo "Counting lines of code in: $(realpath "$TARGET_DIR")"
echo "----------------------------------------"

find "$TARGET_DIR" -type f \( -name "*.rs" -o -name "*.ui" -o -name ".xml" \) -print0 | wc -l --files0-from=-