# Exit codes

The CLI maps core error kinds to stable exit codes:

| Exit code | Error kind | When it occurs |
| --- | --- | --- |
| 1 | Internal | Unexpected bug or crash |
| 2 | Usage | Invalid arguments, malformed input |
| 3 | NotFound | Pool or message doesn't exist |
| 4 | AlreadyExists | Creating a pool that already exists |
| 5 | Busy | Lock contention, resource temporarily unavailable |
| 6 | Permission | Auth failure or access mode violation |
| 7 | Corrupt | Pool file is damaged or invalid |
| 8 | Io | File system or network error |

Additional non-error exit codes:

| Exit code | Condition |
| --- | --- |
| 124 | `peek --timeout` elapsed with no output |

