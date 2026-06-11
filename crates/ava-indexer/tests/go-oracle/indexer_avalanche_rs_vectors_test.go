// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

package indexer

// Indexer golden-vector emitter for the avalanche-rs `ava-indexer` M8.24
// differential test (`tests/differential_indexer_parity.rs::indexer_parity`).
//
// This is the Go oracle: it drives the real indexer through a fixed, fully
// deterministic scenario (fixed chain ids, fixed container ids/bytes, a fixed
// mockable clock) and dumps
//
//   - codec vectors: `Codec.Marshal` bytes for Containers with varied
//     timestamps (byte-exactness of the persisted value format);
//   - the COMPLETE physical database state (every memdb key/value, hex) after
//     run 1 (indexing two chains, one DAG) and run 2 (indexing disabled,
//     incomplete allowed -> incomplete marker) — this pins the sha256-hashed
//     prefixdb namespacing, the versiondb passthrough, the hasRun /
//     previously-indexed / incomplete marker keys, and every index record;
//   - computed query replies: `FormattedContainer` JSON (via
//     `newFormattedContainer` + `json.Marshal`, i.e. the exact service reply
//     shape), range windows, and the error strings of every reachable
//     index-level failure;
//   - the run-3 fatal: re-enabling indexing over an incomplete index with
//     `index-allow-incomplete=false` closes the indexer.
//
// Gated behind `AVAX_RS_OUT=<output-file>` so a normal `go test` run never
// executes it. The committed source-of-truth copy of this file lives in the
// avalanche-rs repo under `crates/ava-indexer/tests/go-oracle/`; drop it into
// the avalanchego `indexer/` package to run (it touches the unexported
// `indexer`/`index` internals), then DELETE it:
//
//	cp crates/ava-indexer/tests/go-oracle/indexer_avalanche_rs_vectors_test.go \
//	   "$AVALANCHEGO_DIR/indexer/"
//	cd "$AVALANCHEGO_DIR"
//	AVAX_RS_GO_COMMIT=$(git rev-parse HEAD) \
//	AVAX_RS_OUT=/abs/crates/ava-indexer/tests/vectors/indexer/indexer_parity.json \
//	  go test -tags test -run TestEmitAvalancheRsIndexerVectors ./indexer/ -count=1
//	rm "$AVALANCHEGO_DIR/indexer/indexer_avalanche_rs_vectors_test.go"

import (
	"encoding/hex"
	"encoding/json"
	"os"
	"sort"
	"testing"
	"time"

	"github.com/stretchr/testify/require"
	"go.uber.org/mock/gomock"

	"github.com/ava-labs/avalanchego/database/memdb"
	"github.com/ava-labs/avalanchego/database/versiondb"
	"github.com/ava-labs/avalanchego/ids"
	"github.com/ava-labs/avalanchego/snow"
	"github.com/ava-labs/avalanchego/snow/engine/avalanche/vertex/vertexmock"
	"github.com/ava-labs/avalanchego/snow/engine/snowman/block/blockmock"
	"github.com/ava-labs/avalanchego/snow/snowtest"
	"github.com/ava-labs/avalanchego/utils/constants"
	"github.com/ava-labs/avalanchego/utils/formatting"
	"github.com/ava-labs/avalanchego/utils/logging"
)

type emittedKV struct {
	KeyHex   string `json:"keyHex"`
	ValueHex string `json:"valueHex"`
}

type emittedContainer struct {
	ID       string `json:"id"`
	BytesHex string `json:"bytesHex"`
}

type emittedCodecVector struct {
	ID         string `json:"id"`
	BytesHex   string `json:"bytesHex"`
	Timestamp  int64  `json:"timestamp"`
	EncodedHex string `json:"encodedHex"`
}

type emittedQueries struct {
	LastAcceptedJSON      json.RawMessage `json:"lastAcceptedJSON"`
	ContainerByIndex1JSON json.RawMessage `json:"containerByIndex1JSON"`
	ContainerByID0JSON    json.RawMessage `json:"containerByID0JSON"`
	Range02JSON           json.RawMessage `json:"range02JSON"`
	RangeAllIDs           []string        `json:"rangeAllIDs"`
	RangeErrTooMany       string          `json:"rangeErrTooMany"`
	RangeErrZero          string          `json:"rangeErrZero"`
	RangeErrPastEnd       string          `json:"rangeErrPastEnd"`
	GetIndexUnknownErr    string          `json:"getIndexUnknownErr"`
	UnknownID             string          `json:"unknownID"`
}

type emittedVectors struct {
	GoCommit         string                        `json:"goCommit"`
	Emitter          string                        `json:"emitter"`
	ClockUnixNanos   int64                         `json:"clockUnixNanos"`
	Chain1ID         string                        `json:"chain1ID"`
	Chain2ID         string                        `json:"chain2ID"`
	Containers       map[string][]emittedContainer `json:"containers"`
	CodecVectors     []emittedCodecVector          `json:"codecVectors"`
	EmptyIndexErrors map[string]string             `json:"emptyIndexErrors"`
	Queries          emittedQueries                `json:"queries"`
	DBDumpAfterRun1  []emittedKV                   `json:"dbDumpAfterRun1"`
	DBDumpAfterRun2  []emittedKV                   `json:"dbDumpAfterRun2"`
	Run3FatalClosed  bool                          `json:"run3FatalClosed"`
}

func fillID(b byte) ids.ID {
	var id ids.ID
	for i := range id {
		id[i] = b
	}
	return id
}

func makeBytes(kind byte, i int) []byte {
	out := make([]byte, 8+i)
	for j := range out {
		out[j] = kind + byte(i) + byte(j)
	}
	return out
}

func parityCtx(chainID ids.ID) *snow.ConsensusContext {
	ctx := snowtest.ConsensusContext(&snow.Context{
		ChainID:  chainID,
		SubnetID: constants.PrimaryNetworkID,
		Log:      logging.NoLog{},
	})
	return ctx
}

func dumpDB(t *testing.T, db *memdb.Database) []emittedKV {
	t.Helper()
	it := db.NewIterator()
	defer it.Release()
	var out []emittedKV
	for it.Next() {
		out = append(out, emittedKV{
			KeyHex:   hex.EncodeToString(it.Key()),
			ValueHex: hex.EncodeToString(it.Value()),
		})
	}
	require.NoError(t, it.Error())
	sort.Slice(out, func(i, j int) bool { return out[i].KeyHex < out[j].KeyHex })
	return out
}

func TestEmitAvalancheRsIndexerVectors(t *testing.T) {
	outPath := os.Getenv("AVAX_RS_OUT")
	if outPath == "" {
		t.Skip("AVAX_RS_OUT not set; skipping the avalanche-rs vector emitter")
	}
	require := require.New(t)
	ctrl := gomock.NewController(t)

	// FormattedContainer timestamps render through time.Unix (local zone);
	// pin UTC so the JSON is deterministic.
	time.Local = time.UTC

	clockTime := time.Unix(1_700_000_000, 123_456_789)
	chain1ID := fillID(0xC1)
	chain2ID := fillID(0xC2)
	unknownID := fillID(0xEE)

	out := emittedVectors{
		GoCommit:         os.Getenv("AVAX_RS_GO_COMMIT"),
		Emitter:          "indexer_avalanche_rs_vectors_test.go",
		ClockUnixNanos:   clockTime.UnixNano(),
		Chain1ID:         chain1ID.String(),
		Chain2ID:         chain2ID.String(),
		Containers:       map[string][]emittedContainer{},
		EmptyIndexErrors: map[string]string{},
	}
	require.NotEmpty(out.GoCommit, "set AVAX_RS_GO_COMMIT for provenance")

	// ---- codec vectors: persisted Container value bytes -----------------
	for _, ts := range []int64{0, 1, clockTime.UnixNano(), 1 << 62} {
		c := Container{
			ID:        fillID(0xAA),
			Bytes:     makeBytes(0x05, 3),
			Timestamp: ts,
		}
		encoded, err := Codec.Marshal(CodecVersion, c)
		require.NoError(err)
		out.CodecVectors = append(out.CodecVectors, emittedCodecVector{
			ID:         c.ID.String(),
			BytesHex:   hex.EncodeToString(c.Bytes),
			Timestamp:  ts,
			EncodedHex: hex.EncodeToString(encoded),
		})
	}

	// ---- run 1: index a Snowman chain + a DAG chain ----------------------
	baseDB := memdb.New()
	db1 := versiondb.New(baseDB)
	config := Config{
		DB:                   db1,
		Log:                  logging.NoLog{},
		IndexingEnabled:      true,
		AllowIncompleteIndex: false,
		BlockAcceptorGroup:   snow.NewAcceptorGroup(logging.NoLog{}),
		TxAcceptorGroup:      snow.NewAcceptorGroup(logging.NoLog{}),
		VertexAcceptorGroup:  snow.NewAcceptorGroup(logging.NoLog{}),
		APIServer:            &apiServerMock{},
		ShutdownF:            func() {},
	}
	idxrIntf, err := NewIndexer(config)
	require.NoError(err)
	idxr := idxrIntf.(*indexer)
	idxr.clock.Set(clockTime)

	chain1Ctx := parityCtx(chain1ID)
	chain2Ctx := parityCtx(chain2ID)

	idxr.RegisterChain("chain1", chain1Ctx, blockmock.NewChainVM(ctrl))
	idxr.RegisterChain("chain2", chain2Ctx, vertexmock.NewLinearizableVM(ctrl))

	// Empty-index error strings (chain2's block index, before any accept).
	chain2BlkIdx := idxr.blockIndices[chain2ID]
	require.NotNil(chain2BlkIdx)
	_, err = chain2BlkIdx.GetLastAccepted()
	require.Error(err)
	out.EmptyIndexErrors["getLastAccepted"] = err.Error()
	_, err = chain2BlkIdx.GetContainerByIndex(0)
	require.Error(err)
	out.EmptyIndexErrors["getContainerByIndex0"] = err.Error()
	_, err = chain2BlkIdx.GetContainerRange(0, 1)
	require.Error(err)
	out.EmptyIndexErrors["getContainerRange01"] = err.Error()

	accept := func(group snow.AcceptorGroup, ctx *snow.ConsensusContext, key string, idFill byte, n int) {
		for i := 0; i < n; i++ {
			id := fillID(idFill + byte(i))
			bytes := makeBytes(idFill, i)
			require.NoError(group.Accept(ctx, id, bytes))
			out.Containers[key] = append(out.Containers[key], emittedContainer{
				ID:       id.String(),
				BytesHex: hex.EncodeToString(bytes),
			})
		}
	}
	accept(config.BlockAcceptorGroup, chain1Ctx, "chain1Blocks", 0x10, 5)
	accept(config.BlockAcceptorGroup, chain2Ctx, "chain2Blocks", 0x20, 2)
	accept(config.VertexAcceptorGroup, chain2Ctx, "chain2Vtxs", 0x30, 3)
	accept(config.TxAcceptorGroup, chain2Ctx, "chain2Txs", 0x40, 3)

	// ---- computed query replies over chain1's block index ----------------
	blkIdx := idxr.blockIndices[chain1ID]
	require.NotNil(blkIdx)

	marshalFC := func(c Container) json.RawMessage {
		idx, err := blkIdx.GetIndex(c.ID)
		require.NoError(err)
		fc, err := newFormattedContainer(c, idx, formatting.Hex)
		require.NoError(err)
		raw, err := json.Marshal(fc)
		require.NoError(err)
		return raw
	}

	last, err := blkIdx.GetLastAccepted()
	require.NoError(err)
	out.Queries.LastAcceptedJSON = marshalFC(last)

	byIndex1, err := blkIdx.GetContainerByIndex(1)
	require.NoError(err)
	out.Queries.ContainerByIndex1JSON = marshalFC(byIndex1)

	byID0, err := blkIdx.GetContainerByID(fillID(0x10))
	require.NoError(err)
	out.Queries.ContainerByID0JSON = marshalFC(byID0)

	rangeContainers, err := blkIdx.GetContainerRange(0, 2)
	require.NoError(err)
	rangeReply := GetContainerRangeResponse{
		Containers: make([]FormattedContainer, len(rangeContainers)),
	}
	for i, c := range rangeContainers {
		idx, err := blkIdx.GetIndex(c.ID)
		require.NoError(err)
		rangeReply.Containers[i], err = newFormattedContainer(c, idx, formatting.Hex)
		require.NoError(err)
	}
	out.Queries.Range02JSON, err = json.Marshal(rangeReply)
	require.NoError(err)

	all, err := blkIdx.GetContainerRange(0, MaxFetchedByRange)
	require.NoError(err)
	for _, c := range all {
		out.Queries.RangeAllIDs = append(out.Queries.RangeAllIDs, c.ID.String())
	}

	_, err = blkIdx.GetContainerRange(0, MaxFetchedByRange+1)
	require.Error(err)
	out.Queries.RangeErrTooMany = err.Error()
	_, err = blkIdx.GetContainerRange(0, 0)
	require.Error(err)
	out.Queries.RangeErrZero = err.Error()
	_, err = blkIdx.GetContainerRange(9, 1)
	require.Error(err)
	out.Queries.RangeErrPastEnd = err.Error()
	_, err = blkIdx.GetIndex(unknownID)
	require.Error(err)
	out.Queries.GetIndexUnknownErr = err.Error()
	out.Queries.UnknownID = unknownID.String()

	// ---- commit + close + full physical dump ------------------------------
	require.NoError(db1.Commit())
	require.NoError(idxr.Close())
	out.DBDumpAfterRun1 = dumpDB(t, baseDB)

	// ---- run 2: indexing disabled, incomplete allowed -> marker -----------
	db2 := versiondb.New(baseDB)
	config.DB = db2
	config.IndexingEnabled = false
	config.AllowIncompleteIndex = true
	idxrIntf, err = NewIndexer(config)
	require.NoError(err)
	idxr = idxrIntf.(*indexer)
	idxr.RegisterChain("chain1", parityCtx(chain1ID), blockmock.NewChainVM(ctrl))
	isIncomplete, err := idxr.isIncomplete(chain1ID)
	require.NoError(err)
	require.True(isIncomplete)
	require.NoError(db2.Commit())
	require.NoError(idxr.Close())
	out.DBDumpAfterRun2 = dumpDB(t, baseDB)

	// ---- run 3: indexing re-enabled, incomplete disallowed -> fatal -------
	db3 := versiondb.New(baseDB)
	config.DB = db3
	config.IndexingEnabled = true
	config.AllowIncompleteIndex = false
	idxrIntf, err = NewIndexer(config)
	require.NoError(err)
	idxr = idxrIntf.(*indexer)
	idxr.RegisterChain("chain1", parityCtx(chain1ID), blockmock.NewChainVM(ctrl))
	out.Run3FatalClosed = idxr.closed
	require.True(out.Run3FatalClosed)

	raw, err := json.MarshalIndent(out, "", "  ")
	require.NoError(err)
	require.NoError(os.WriteFile(outPath, append(raw, '\n'), 0o644))
	t.Logf("wrote avalanche-rs indexer vectors to %s", outPath)
}
