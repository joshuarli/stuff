#!/usr/bin/env python3

from functools import cmp_to_key

# one linear pass. no memory copying


def comparator(x, y):
    # TODO: replace with x_start, x_end for readability
    xi, xj, yi, yj = 0, 0, 0, 0
    xs, ys = x[xj] in "0123456789", y[yj] in "0123456789"  # state - true if numeric

    while True:
        # scan chunk in x
        while xj < len(x) and ((x[xj] in "0123456789") == xs):
            xj += 1

        # scan chunk in y
        while yj < len(y) and ((y[yj] in "0123456789") == ys):
            yj += 1

        lx, ly = xj - xi, yj - yi

        if lx == ly == 0:
            break

        if lx == 0:
            return -1

        if ly == 0:
            return 1

        # make sure this isn't copied. like go's slice views
        cx = x[xi:xj]
        cy = y[yi:yj]

        if xs:
            cx = int(cx)
            if ys:
                cy = int(cy)
                # numerical comparison
                # hmm... would it be worth adding a == here to sidestep 2 more comparisons?
                # depends on if == is common case.
                # also, would need to duplicate bookkeeping to achieve this.
                if cx < cy:
                    return -1
                if cx > cy:
                    return 1
            else:
                # numerical chunks are always less than alphanumeric
                return -1
        else:
            if ys:
                # numerical chunks are always less than alphanumeric
                return 1
            else:
                # string comparison
                # XXX: This is not i18nalized.
                if cx < cy:
                    return -1
                if cx > cy:
                    return 1

        # update bookkeeping
        xs, ys = not xs, not ys
        xi, yi = xj, yj

    return 0


import sys

data = list(map(str.strip, sys.stdin.readlines()))
data.sort(key=cmp_to_key(comparator))
print("\n".join(data))
