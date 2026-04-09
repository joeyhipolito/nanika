#!/bin/bash
# Learning capture hook for -p1
# Domain: 

OUTPUT_FILE="learnings/-p1.json"
mkdir -p "$(dirname "$OUTPUT_FILE")"

while IFS= read -r line; do
    if [[ "$line" == *"LEARNING:"* ]]; then
        content="${line#*LEARNING:}"
        echo '{"marker":"LEARNING:","type":"insight","content":"'"$content"'","timestamp":"'$(date -u +%Y-%m-%dT%H:%M:%SZ)'"}' >> "$OUTPUT_FILE"
    fi
    if [[ "$line" == *"TIL:"* ]]; then
        content="${line#*TIL:}"
        echo '{"marker":"TIL:","type":"insight","content":"'"$content"'","timestamp":"'$(date -u +%Y-%m-%dT%H:%M:%SZ)'"}' >> "$OUTPUT_FILE"
    fi
    if [[ "$line" == *"INSIGHT:"* ]]; then
        content="${line#*INSIGHT:}"
        echo '{"marker":"INSIGHT:","type":"insight","content":"'"$content"'","timestamp":"'$(date -u +%Y-%m-%dT%H:%M:%SZ)'"}' >> "$OUTPUT_FILE"
    fi
    if [[ "$line" == *"FINDING:"* ]]; then
        content="${line#*FINDING:}"
        echo '{"marker":"FINDING:","type":"insight","content":"'"$content"'","timestamp":"'$(date -u +%Y-%m-%dT%H:%M:%SZ)'"}' >> "$OUTPUT_FILE"
    fi
    if [[ "$line" == *"PATTERN:"* ]]; then
        content="${line#*PATTERN:}"
        echo '{"marker":"PATTERN:","type":"pattern","content":"'"$content"'","timestamp":"'$(date -u +%Y-%m-%dT%H:%M:%SZ)'"}' >> "$OUTPUT_FILE"
    fi
    if [[ "$line" == *"APPROACH:"* ]]; then
        content="${line#*APPROACH:}"
        echo '{"marker":"APPROACH:","type":"pattern","content":"'"$content"'","timestamp":"'$(date -u +%Y-%m-%dT%H:%M:%SZ)'"}' >> "$OUTPUT_FILE"
    fi
    if [[ "$line" == *"GOTCHA:"* ]]; then
        content="${line#*GOTCHA:}"
        echo '{"marker":"GOTCHA:","type":"error","content":"'"$content"'","timestamp":"'$(date -u +%Y-%m-%dT%H:%M:%SZ)'"}' >> "$OUTPUT_FILE"
    fi
    if [[ "$line" == *"FIX:"* ]]; then
        content="${line#*FIX:}"
        echo '{"marker":"FIX:","type":"error","content":"'"$content"'","timestamp":"'$(date -u +%Y-%m-%dT%H:%M:%SZ)'"}' >> "$OUTPUT_FILE"
    fi
    if [[ "$line" == *"SOURCE:"* ]]; then
        content="${line#*SOURCE:}"
        echo '{"marker":"SOURCE:","type":"source","content":"'"$content"'","timestamp":"'$(date -u +%Y-%m-%dT%H:%M:%SZ)'"}' >> "$OUTPUT_FILE"
    fi
    if [[ "$line" == *"DECISION:"* ]]; then
        content="${line#*DECISION:}"
        echo '{"marker":"DECISION:","type":"decision","content":"'"$content"'","timestamp":"'$(date -u +%Y-%m-%dT%H:%M:%SZ)'"}' >> "$OUTPUT_FILE"
    fi
    if [[ "$line" == *"TRADEOFF:"* ]]; then
        content="${line#*TRADEOFF:}"
        echo '{"marker":"TRADEOFF:","type":"decision","content":"'"$content"'","timestamp":"'$(date -u +%Y-%m-%dT%H:%M:%SZ)'"}' >> "$OUTPUT_FILE"
    fi
done
