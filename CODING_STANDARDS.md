# Coding standards

## Code comments

Use code comments extremely sparingly. Omit anything inferable from any file in the project.

Add a comment only to record information obtained outside the project that is absent from its files, and only when omitting it would lead a human or agent to an obvious mistake when reading or editing the code.

## Shell scripts

Use POSIX `sh` for shell scripts. Use Bash only when POSIX `sh` cannot reasonably meet the script's requirements.

Name POSIX `sh` scripts with a `.sh` suffix and Bash scripts with a `.bash` suffix. Use a shebang that selects the corresponding interpreter.
