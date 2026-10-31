import filecmp
import os
from pathlib import Path
import stat
import sys


def equal_trees(left, right):
    left_entries = {entry.name: entry for entry in left.iterdir()}
    right_entries = {entry.name: entry for entry in right.iterdir()}
    if left_entries.keys() != right_entries.keys():
        return False
    for name, source in left_entries.items():
        destination = right_entries[name]
        source_mode = source.lstat().st_mode
        destination_mode = destination.lstat().st_mode
        if source_mode != destination_mode:
            return False
        if stat.S_ISLNK(source_mode):
            if os.readlink(source) != os.readlink(destination):
                return False
        elif stat.S_ISDIR(source_mode):
            if not equal_trees(source, destination):
                return False
        elif stat.S_ISREG(source_mode):
            if not filecmp.cmp(source, destination, shallow=False):
                return False
        else:
            raise ValueError(f'Unsupported artifact entry: {source}')
    return True


if __name__ == '__main__':
    try:
        equal = equal_trees(Path(sys.argv[1]), Path(sys.argv[2]))
    except (OSError, ValueError) as error:
        print(error, file=sys.stderr)
        sys.exit(2)
    sys.exit(0 if equal else 1)
