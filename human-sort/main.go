package main

import (
	"bufio"
	"fmt"
	"os"
	"sort"
	"strconv"
)

func isdigit(b byte) bool { return '0' <= b && b <= '9' }

// caveats:
// does unnecessary int parsing; utf-8 compares bytewise-lexicographically
//   - and so is inferior to fvbommel's natsort
// only considers ascii digits 0-9 as numerics
// does not deal with negative or decimal representations
// does not consider leading zeroes as a tie-breaker

func alphanumericLess(x, y string) bool {
	if len(x) == 0 {
		return false
	}
	if len(y) == 0 {
		return true
	}
	xStart, xEnd, yStart, yEnd := 0, 0, 0, 0
	xState := isdigit(x[xEnd]) // state is true if numeric
	yState := isdigit(y[yEnd])

	var xLen, yLen int
	var xChunk, yChunk string
	var xChunkNval, yChunkNval uint64
	var err error
	for {
		// scan chunk in x (advance xEnd until state flips)
		for (xEnd < len(x)) && (isdigit(x[xEnd]) == xState) {
			xEnd++
		}
		// ditto for y
		for (yEnd < len(y)) && (isdigit(y[yEnd]) == yState) {
			yEnd++
		}

		xLen, yLen = xEnd-xStart, yEnd-yStart

		if xLen == 0 {
			return true
		}
		if yLen == 0 {
			return false
		} // x >= y

		// hmm... is this copied? because these are string types and it won't let me
		// []string
		xChunk = x[xStart:xEnd]
		yChunk = y[yStart:yEnd]

		if xState {
			xChunkNval, err = strconv.ParseUint(xChunk, 10, 64)
			if err != nil {
				panic(err) // lol
			}
			if !yState {
				// numerical chunks are always less than alphanumeric
				return true
			}

			// numerical comparison
			yChunkNval, err = strconv.ParseUint(yChunk, 10, 64)
			if err != nil {
				panic(err) // lol
			}
			if xChunkNval < yChunkNval {
				return true
			}
			if xChunkNval > yChunkNval {
				return false
			}
		} else {
			if yState {
				// numerical chunks are always less than alphanumeric
				return false
			}

			// string comparison
			// XXX: this is not i18n'd
			if xChunk < yChunk {
				return true
			}
			if xChunk > yChunk {
				return false
			}
		}

		// update bookkeeping
		xState, yState = !xState, !yState
		xStart, yStart = xEnd, yEnd
	}

	return false // x == y
}

func main() {
	var buf []string
	scanner := bufio.NewScanner(os.Stdin)
	for scanner.Scan() {
		buf = append(buf, scanner.Text())
	}
	sort.Slice(buf, func(i, j int) bool {
		return alphanumericLess(buf[i], buf[j])
	})
	for _, line := range buf {
		fmt.Println(line)
	}
}
