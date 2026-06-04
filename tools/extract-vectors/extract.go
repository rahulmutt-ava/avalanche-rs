// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

// Command extract-vectors dumps golden test vectors from a pinned avalanchego
// tree for the avalanche-rs parity tests. See README.md.
//
// HOW TO RUN: this program imports avalanchego packages, so it is run from
// WITHIN the avalanchego module (it is not part of the Cargo workspace and is
// excluded from any Go build here via the `ignore` tag). Copy it into a fresh
// package dir under the pinned avalanchego checkout and run it:
//
//	cp tools/extract-vectors/extract.go ~/avalanchego/cmd_extract_vectors/main.go
//	cd ~/avalanchego && go run ./cmd_extract_vectors \
//	    --out /path/to/avalanche-rs/tests/vectors
//	rm -rf ~/avalanchego/cmd_extract_vectors   # leave avalanchego clean
//
// The committed vectors under tests/vectors/ were produced from avalanchego
// revision fb174e8925ba86e9ba5fd84eb4d6e5e8c23ffc11 (see tests/vectors/manifest.json).
//
// Surfaces still TODO: the linear-codec golden bytes (tests/vectors/codec/) must
// be produced alongside the Rust type registry (M0.15/M0.16) so the Go structs
// mirror the Rust types exactly; and the `large_rsa_key` REJECT cert for M0.20.
//
//go:build ignore

package main

import (
	"encoding/hex"
	"encoding/json"
	"flag"
	"fmt"
	"math"
	"math/big"
	"os"
	"path/filepath"
	"time"

	"gonum.org/v1/gonum/mathext/prng"

	"github.com/ava-labs/avalanchego/ids"
	"github.com/ava-labs/avalanchego/staking"
	"github.com/ava-labs/avalanchego/upgrade"
	"github.com/ava-labs/avalanchego/utils/cb58"
	"github.com/ava-labs/avalanchego/utils/constants"
	"github.com/ava-labs/avalanchego/utils/crypto/bls"
	"github.com/ava-labs/avalanchego/utils/crypto/bls/signer/localsigner"
	"github.com/ava-labs/avalanchego/utils/crypto/secp256k1"
	"github.com/ava-labs/avalanchego/utils/formatting"
	"github.com/ava-labs/avalanchego/utils/formatting/address"
	"github.com/ava-labs/avalanchego/utils/hashing"
	"github.com/ava-labs/avalanchego/utils/sampler"
)

const avalanchegoRevision = "fb174e8925ba86e9ba5fd84eb4d6e5e8c23ffc11"

var seeds = []uint64{0, 1, 5489, 0xDEADBEEF, math.MaxUint64, 1700000000000000000}

func main() {
	out := flag.String("out", "", "output dir (avalanche-rs/tests/vectors)")
	flag.Parse()
	if *out == "" {
		fmt.Fprintln(os.Stderr, "usage: go run ./cmd_extract_vectors --out <tests/vectors>")
		os.Exit(2)
	}
	dumpRNG(*out)
	dumpUint64Inclusive(*out)
	dumpSamplers(*out)
	dumpIDStrings(*out)
	dumpCB58Raw(*out)
	dumpAddr(*out)
	dumpEncodings(*out)
	dumpSecp(*out)
	dumpBLS(*out)
	dumpNodeID(*out)
	dumpUpgrade(*out)
	fmt.Println("extract-vectors: done")
}

// ---- helpers ---------------------------------------------------------------

func write(out, surface, name string, v any) {
	dir := filepath.Join(out, surface)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		panic(err)
	}
	b, err := json.MarshalIndent(v, "", "  ")
	if err != nil {
		panic(err)
	}
	p := filepath.Join(dir, name)
	if err := os.WriteFile(p, append(b, '\n'), 0o644); err != nil {
		panic(err)
	}
	fmt.Printf("  wrote %s/%s\n", surface, name)
}

func prov(extra map[string]any) map[string]any {
	m := map[string]any{
		"_provenance": map[string]string{
			"avalanchego_revision": avalanchegoRevision,
			"extracted_by":         "tools/extract-vectors/extract.go",
		},
	}
	for k, v := range extra {
		m[k] = v
	}
	return m
}

func hx(b []byte) string { return hex.EncodeToString(b) }

// ---- RNG (R1 gate) ---------------------------------------------------------

type streamVec struct {
	Seed   uint64   `json:"seed"`
	Stream []uint64 `json:"stream"`
}

func dumpRNG(out string) {
	var v64, v32 []streamVec
	for _, s := range seeds {
		g := prng.NewMT19937_64()
		g.Seed(s)
		st := make([]uint64, 320)
		for i := range st {
			st[i] = g.Uint64()
		}
		v64 = append(v64, streamVec{Seed: s, Stream: st})

		h := prng.NewMT19937()
		h.Seed(s)
		st2 := make([]uint64, 320)
		for i := range st2 {
			st2[i] = h.Uint64()
		}
		v32 = append(v32, streamVec{Seed: s, Stream: st2})
	}
	write(out, "rng", "mt19937_64.json", v64)
	write(out, "rng", "mt19937_32.json", v32)
}

// ---- Uint64Inclusive + samplers --------------------------------------------
// Uint64Inclusive is unexported (a method on the internal `rng`). Its first
// draw is reachable through NewDeterministicUniform: with length n+1 and
// drawsCount 0, the first Sample(1) result equals Uint64Inclusive(n). We emit a
// faithful single value per (seed, n) covering the three branches.

type u64incVec struct {
	Seed   uint64   `json:"seed"`
	N      uint64   `json:"n"`
	Output []uint64 `json:"outputs"`
}

func dumpUint64Inclusive(out string) {
	ns := []uint64{255, math.MaxUint64 - 1, 10}
	var vecs []u64incVec
	for _, s := range seeds {
		for _, n := range ns {
			src := prng.NewMT19937_64()
			src.Seed(s)
			u := sampler.NewDeterministicUniform(src)
			length := n
			if n < math.MaxUint64 {
				length = n + 1
			}
			u.Initialize(length)
			got, ok := u.Sample(1)
			if !ok {
				continue
			}
			vecs = append(vecs, u64incVec{Seed: s, N: n, Output: got})
		}
	}
	write(out, "sampler", "uint64_inclusive.json", prov(map[string]any{"cases": vecs}))
}

type samplerVec struct {
	Kind           string   `json:"kind"`
	Seed           uint64   `json:"seed"`
	Weights        []uint64 `json:"weights,omitempty"`
	Length         uint64   `json:"length,omitempty"`
	Count          int      `json:"count,omitempty"`
	SampleValues   []uint64 `json:"sample_values,omitempty"`
	SampledIndices []int    `json:"sampled_indices"`
}

func dumpSamplers(out string) {
	var vecs []samplerVec

	for _, s := range seeds[:3] {
		src := prng.NewMT19937_64()
		src.Seed(s)
		u := sampler.NewDeterministicUniform(src)
		u.Initialize(20)
		got, _ := u.Sample(8)
		idx := make([]int, len(got))
		for i, g := range got {
			idx[i] = int(g)
		}
		vecs = append(vecs, samplerVec{Kind: "uniform", Seed: s, Length: 20, Count: 8, SampledIndices: idx})
	}

	{
		weights := []uint64{1, 2, 3, 4, 10}
		w := sampler.NewWeighted()
		_ = w.Initialize(weights)
		vals := []uint64{0, 1, 3, 6, 10, 15, 19}
		var idx []int
		for _, v := range vals {
			i, ok := w.Sample(v)
			if !ok {
				idx = append(idx, -1)
				continue
			}
			idx = append(idx, i)
		}
		vecs = append(vecs, samplerVec{Kind: "weighted", Weights: weights, SampleValues: vals, SampledIndices: idx})
	}

	for _, s := range seeds[:3] {
		weights := []uint64{1, 2, 3, 4, 10}
		src := prng.NewMT19937_64()
		src.Seed(s)
		wwr := sampler.NewDeterministicWeightedWithoutReplacement(src)
		_ = wwr.Initialize(weights)
		idx, _ := wwr.Sample(3)
		vecs = append(vecs, samplerVec{Kind: "wwr", Seed: s, Weights: weights, Count: 3, SampledIndices: idx})
	}

	write(out, "sampler", "samplers.json", prov(map[string]any{"cases": vecs}))
}

// ---- ids / CB58 ------------------------------------------------------------

type idStr struct {
	Kind     string `json:"kind"`
	BytesHex string `json:"bytes_hex"`
	String   string `json:"string"`
}

func dumpIDStrings(out string) {
	var vecs []idStr
	id32 := func(b []byte) ids.ID { id, _ := ids.ToID(b); return id }
	short20 := func(b []byte) ids.ShortID { s, _ := ids.ToShortID(b); return s }

	for _, p := range [][]byte{make([]byte, 32), bytesRepeat(0x01, 32), bytesSeq(32), bytesRepeat(0xff, 32)} {
		vecs = append(vecs, idStr{Kind: "id", BytesHex: hx(p), String: id32(p).String()})
	}
	for _, p := range [][]byte{make([]byte, 20), bytesSeq(20), bytesRepeat(0xab, 20)} {
		s := short20(p)
		vecs = append(vecs, idStr{Kind: "short_id", BytesHex: hx(p), String: s.String()})
		vecs = append(vecs, idStr{Kind: "node_id", BytesHex: hx(p), String: ids.NodeID(s).String()})
	}
	write(out, "ids", "cb58.json", vecs)
}

type cb58Pair struct {
	BytesHex string `json:"bytes_hex"`
	CB58     string `json:"cb58"`
}

func dumpCB58Raw(out string) {
	var vecs []cb58Pair
	for _, p := range [][]byte{{}, {0x00}, {0xde, 0xad, 0xbe, 0xef}, bytesSeq(20), bytesSeq(32)} {
		s, err := cb58.Encode(p)
		if err != nil {
			panic(err)
		}
		vecs = append(vecs, cb58Pair{BytesHex: hx(p), CB58: s})
	}
	write(out, "ids", "cb58_raw.json", vecs)
}

// ---- hashing / address derivation ------------------------------------------

type addrVec struct {
	PubkeyHex   string `json:"pubkey_hex"`
	AddressHex  string `json:"address_hex"`
	ChecksumHex string `json:"checksum4_hex"`
}

func dumpAddr(out string) {
	var vecs []addrVec
	for i := byte(1); i <= 3; i++ {
		pk, _ := secp256k1.ToPrivateKey(bytesRepeat(i, 32))
		pub := pk.PublicKey().Bytes()
		vecs = append(vecs, addrVec{
			PubkeyHex:   hx(pub),
			AddressHex:  hx(hashing.PubkeyBytesToAddress(pub)),
			ChecksumHex: hx(hashing.Checksum(pub, 4)),
		})
	}
	write(out, "crypto", "addr.json", vecs)
}

// ---- formatting / bech32 ---------------------------------------------------

type encVec struct {
	PayloadHex string `json:"payload_hex"`
	Hex        string `json:"hex"`
	HexNC      string `json:"hex_nc"`
	HexC       string `json:"hex_c"`
}

type bechVec struct {
	Alias      string `json:"alias"`
	HRP        string `json:"hrp"`
	PayloadHex string `json:"payload_hex"`
	Bech32     string `json:"bech32"`
	Formatted  string `json:"formatted"`
}

func dumpEncodings(out string) {
	var encs []encVec
	for _, p := range [][]byte{{}, {0x00}, {0xde, 0xad, 0xbe, 0xef}, bytesSeq(20)} {
		h, _ := formatting.Encode(formatting.Hex, p)
		hnc, _ := formatting.Encode(formatting.HexNC, p)
		hc, _ := formatting.Encode(formatting.HexC, p)
		encs = append(encs, encVec{PayloadHex: hx(p), Hex: h, HexNC: hnc, HexC: hc})
	}

	var bechs []bechVec
	cases := []struct {
		alias string
		net   uint32
	}{{"X", constants.MainnetID}, {"P", constants.FujiID}, {"C", constants.MainnetID}}
	for _, c := range cases {
		hrp := constants.GetHRP(c.net)
		payload := bytesSeq(20)
		b32, _ := address.FormatBech32(hrp, payload)
		bechs = append(bechs, bechVec{Alias: c.alias, HRP: hrp, PayloadHex: hx(payload), Bech32: b32, Formatted: c.alias + "-" + b32})
	}
	write(out, "crypto", "encodings.json", prov(map[string]any{"hex": encs, "bech32": bechs}))
}

// ---- secp256k1 -------------------------------------------------------------

var secpN, _ = new(big.Int).SetString("fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141", 16)

type secpVec struct {
	PrivHex          string `json:"priv_hex"`
	PrivString       string `json:"priv_string"`
	PubCompressedHex string `json:"pub_compressed_hex"`
	AddressHex       string `json:"address_hex"`
	EthAddressHex    string `json:"eth_address_hex"`
	HashHex          string `json:"hash_hex"`
	SigHex           string `json:"sig_hex"`
	HighSSigHex      string `json:"high_s_sig_hex"`
}

func dumpSecp(out string) {
	var vecs []secpVec
	for i := byte(1); i <= 3; i++ {
		priv, _ := secp256k1.ToPrivateKey(bytesRepeat(i, 32))
		pub := priv.PublicKey()
		hash := hashing.ComputeHash256([]byte{i, i, i})
		sig, err := priv.SignHash(hash)
		if err != nil {
			panic(err)
		}
		rec, err := secp256k1.RecoverPublicKeyFromHash(hash, sig)
		if err != nil || rec.Address() != pub.Address() {
			panic("recover mismatch")
		}
		// High-S variant (must be REJECTED by verify_sig_format): s' = N - s, flip v.
		high := make([]byte, len(sig))
		copy(high, sig)
		s := new(big.Int).SetBytes(sig[32:64])
		copy(high[32:64], new(big.Int).Sub(secpN, s).FillBytes(make([]byte, 32)))
		high[64] ^= 1
		vecs = append(vecs, secpVec{
			PrivHex:          hx(priv.Bytes()),
			PrivString:       priv.String(),
			PubCompressedHex: hx(pub.Bytes()),
			AddressHex:       hx(pub.Address().Bytes()),
			EthAddressHex:    hx(pub.EthAddress().Bytes()),
			HashHex:          hx(hash),
			SigHex:           hx(sig),
			HighSSigHex:      hx(high),
		})
	}
	write(out, "crypto", "secp.json", vecs)
}

// ---- BLS -------------------------------------------------------------------

type blsVec struct {
	SecretHex     string   `json:"secret_hex"`
	PubCompressed string   `json:"pub_compressed_hex"`
	PoPHex        string   `json:"pop_hex"`
	MsgHex        string   `json:"msg_hex"`
	SigHex        string   `json:"sig_hex"`
	AggSecretsHex []string `json:"agg_secrets_hex"`
	AggSigHex     string   `json:"agg_sig_hex"`
	AggPubHex     string   `json:"agg_pub_compressed_hex"`
	DSTSignature  string   `json:"dst_signature"`
	DSTPoP        string   `json:"dst_pop"`
}

func dumpBLS(out string) {
	sk1 := bytesBE(1)
	ls, err := localsigner.FromBytes(sk1)
	if err != nil {
		panic(err)
	}
	pk := ls.PublicKey()
	pkComp := bls.PublicKeyToCompressedBytes(pk)
	pop, err := ls.SignProofOfPossession(pkComp)
	if err != nil {
		panic(err)
	}
	msg := []byte("avalanche-rs golden bls message")
	sig, err := ls.Sign(msg)
	if err != nil {
		panic(err)
	}
	if !bls.Verify(pk, sig, msg) || !bls.VerifyProofOfPossession(pk, pop, pkComp) {
		panic("bls verify failed")
	}

	var aggSecrets [][]byte
	var sigs []*bls.Signature
	var pks []*bls.PublicKey
	for i := byte(2); i <= 4; i++ {
		s := bytesBE(uint64(i))
		l, err := localsigner.FromBytes(s)
		if err != nil {
			panic(err)
		}
		si, err := l.Sign(msg)
		if err != nil {
			panic(err)
		}
		aggSecrets = append(aggSecrets, s)
		sigs = append(sigs, si)
		pks = append(pks, l.PublicKey())
	}
	aggSig, err := bls.AggregateSignatures(sigs)
	if err != nil {
		panic(err)
	}
	aggPub, err := bls.AggregatePublicKeys(pks)
	if err != nil {
		panic(err)
	}

	v := blsVec{
		SecretHex:     hx(sk1),
		PubCompressed: hx(pkComp),
		PoPHex:        hx(bls.SignatureToBytes(pop)),
		MsgHex:        hx(msg),
		SigHex:        hx(bls.SignatureToBytes(sig)),
		AggSigHex:     hx(bls.SignatureToBytes(aggSig)),
		AggPubHex:     hx(bls.PublicKeyToCompressedBytes(aggPub)),
		DSTSignature:  hx(bls.CiphersuiteSignature.Bytes()),
		DSTPoP:        hx(bls.CiphersuiteProofOfPossession.Bytes()),
	}
	for _, s := range aggSecrets {
		v.AggSecretsHex = append(v.AggSecretsHex, hx(s))
	}
	write(out, "crypto", "bls.json", v)

	if err := os.WriteFile(filepath.Join(out, "crypto", "signer.key"), ls.ToBytes(), 0o600); err != nil {
		panic(err)
	}
	fmt.Println("  wrote crypto/signer.key")
}

// ---- NodeID from cert ------------------------------------------------------

type nodeIDVec struct {
	CertDERHex string `json:"cert_der_hex"`
	NodeID     string `json:"node_id"`
}

func dumpNodeID(out string) {
	var vecs []nodeIDVec
	for i := 0; i < 3; i++ {
		tlsCert, err := staking.NewTLSCert()
		if err != nil {
			panic(err)
		}
		der := tlsCert.Certificate[0]
		cert, err := staking.ParseCertificate(der)
		if err != nil {
			panic(err)
		}
		vecs = append(vecs, nodeIDVec{CertDERHex: hx(der), NodeID: ids.NodeIDFromCert(cert).String()})
	}
	write(out, "crypto", "nodeid.json", prov(map[string]any{
		"_note": "ECDSA P-256 staking certs (random per generation). The large_rsa_key REJECT case (RSA-3072) must be added separately for M0.20.",
		"cases": vecs,
	}))
}

// ---- upgrade activation ----------------------------------------------------

type forkSample struct {
	AtRFC3339Nano string `json:"at_rfc3339_nano"`
	IsActive      bool   `json:"is_active"`
}

type forkVec struct {
	Network         string       `json:"network"`
	Fork            string       `json:"fork"`
	ForkTimeRFC3339 string       `json:"fork_time_rfc3339_nano"`
	Samples         []forkSample `json:"samples"`
}

func dumpUpgrade(out string) {
	var vecs []forkVec
	nets := []struct {
		name string
		id   uint32
	}{{"mainnet", constants.MainnetID}, {"fuji", constants.FujiID}}
	for _, net := range nets {
		cfg := upgrade.GetConfig(net.id)
		forks := []struct {
			name string
			t    time.Time
		}{
			{"apricot_phase_1", cfg.ApricotPhase1Time},
			{"apricot_phase_2", cfg.ApricotPhase2Time},
			{"apricot_phase_3", cfg.ApricotPhase3Time},
			{"apricot_phase_4", cfg.ApricotPhase4Time},
			{"apricot_phase_5", cfg.ApricotPhase5Time},
			{"apricot_phase_pre_6", cfg.ApricotPhasePre6Time},
			{"apricot_phase_6", cfg.ApricotPhase6Time},
			{"apricot_phase_post_6", cfg.ApricotPhasePost6Time},
			{"banff", cfg.BanffTime},
			{"cortina", cfg.CortinaTime},
			{"durango", cfg.DurangoTime},
			{"etna", cfg.EtnaTime},
			{"fortuna", cfg.FortunaTime},
			{"granite", cfg.GraniteTime},
			{"helicon", cfg.HeliconTime},
		}
		for _, f := range forks {
			samples := []forkSample{}
			for _, d := range []time.Duration{-1, 0, 1} {
				at := f.t.Add(d)
				samples = append(samples, forkSample{
					AtRFC3339Nano: at.UTC().Format(time.RFC3339Nano),
					IsActive:      !at.Before(f.t),
				})
			}
			vecs = append(vecs, forkVec{
				Network:         net.name,
				Fork:            f.name,
				ForkTimeRFC3339: f.t.UTC().Format(time.RFC3339Nano),
				Samples:         samples,
			})
		}
	}
	write(out, "upgrade", "activation.json", prov(map[string]any{"cases": vecs}))
}

// ---- byte helpers ----------------------------------------------------------

func bytesRepeat(b byte, n int) []byte {
	s := make([]byte, n)
	for i := range s {
		s[i] = b
	}
	return s
}

func bytesSeq(n int) []byte {
	s := make([]byte, n)
	for i := range s {
		s[i] = byte(i)
	}
	return s
}

// bytesBE returns the 32-byte big-endian encoding of v (a BLS secret-key scalar).
func bytesBE(v uint64) []byte {
	b := make([]byte, 32)
	new(big.Int).SetUint64(v).FillBytes(b)
	return b
}
