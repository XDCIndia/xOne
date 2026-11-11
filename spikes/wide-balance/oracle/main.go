// Command oracle prints geth-XDC wide-balance vectors as JSON.
//
//	oracle encoding  -> boundary encodings
//	oracle matrix    -> the rule and revert matrix
//	oracle replay    -> run one row (JSON on stdin) at a historical block number
package main

import (
	"encoding/json"
	"fmt"
	"math/big"
	"os"
)

// Largest true balance observed on Apothem: 0xb6c1f9da.. between blocks 31,229,106 and 34,567,914 (REPORT.md, "Legal range").
const apothemMax = "2ffffffffffffffffffffffffffffffffffffffe6c1949996b3605008e391759c"

func main() {
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "usage: oracle encoding|matrix|replay")
		os.Exit(2)
	}
	var v any
	switch os.Args[1] {
	case "encoding":
		m, _ := new(big.Int).SetString(apothemMax, 16)
		v = encodingVectors(m)
	case "matrix":
		v = matrixVectors()
	case "replay":
		var r Row
		must(json.NewDecoder(os.Stdin).Decode(&r))
		runRow(&r)
		v = r.Expect
	default:
		fmt.Fprintln(os.Stderr, "unknown mode")
		os.Exit(2)
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	must(enc.Encode(v))
}
