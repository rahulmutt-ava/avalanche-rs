// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

// Command tls_handshake is the isolated TLS-1.3 interop harness for
// avalanche-rs M9.15. It stands up one side of a mutually-authenticated TLS
// 1.3 handshake using avalanchego's real crypto/tls config (network/peer +
// staking) and reports the verbatim Handshake() result as a single JSON line
// on stdout. Paired against the Rust ava_network Upgrader by the
// ava-differential tls_repro driver to localize the live mixed_network stall.
//
// Canonical source lives in the avalanche-rs repo under
// tests/differential/go-oracle/tls_handshake/; it is copied into
// $AVALANCHEGO_SRC by the Rust driver so it compiles against that checkout's
// avalanchego packages. NOT part of the avalanchego build.
package main

import (
	"crypto/ecdsa"
	"crypto/rsa"
	"crypto/tls"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"time"

	"github.com/ava-labs/avalanchego/network/peer"
	"github.com/ava-labs/avalanchego/staking"
)

type outcome struct {
	OK          bool   `json:"ok"`
	Error       string `json:"error,omitempty"`
	Version     uint16 `json:"version,omitempty"`
	CipherSuite uint16 `json:"cipher_suite,omitempty"`
	PeerCertLen int    `json:"peer_cert_len,omitempty"`
	PeerKeyType string `json:"peer_key_type,omitempty"`
}

func emit(o outcome) {
	b, _ := json.Marshal(o)
	fmt.Fprintln(os.Stdout, string(b))
}

func fatalf(format string, a ...any) {
	fmt.Fprintf(os.Stderr, format+"\n", a...)
	os.Exit(2)
}

func loadCert(keytype string) tls.Certificate {
	switch keytype {
	case "ecdsa":
		certPEM, keyPEM, err := staking.NewCertAndKeyBytes()
		if err != nil {
			fatalf("NewCertAndKeyBytes: %v", err)
		}
		cert, err := tls.X509KeyPair(certPEM, keyPEM)
		if err != nil {
			fatalf("X509KeyPair(ecdsa): %v", err)
		}
		return cert
	case "rsa":
		src := os.Getenv("AVALANCHEGO_SRC")
		if src == "" {
			src = filepath.Join(os.Getenv("HOME"), "avalanchego")
		}
		dir := filepath.Join(src, "staking", "local")
		cert, err := staking.LoadTLSCertFromFiles(
			filepath.Join(dir, "staker1.key"),
			filepath.Join(dir, "staker1.crt"),
		)
		if err != nil {
			fatalf("LoadTLSCertFromFiles(rsa fixture): %v", err)
		}
		return *cert
	default:
		fatalf("unknown --keytype %q (want ecdsa|rsa)", keytype)
		return tls.Certificate{}
	}
}

func tlsConfig(cert tls.Certificate, verify string, keyLog io.Writer) *tls.Config {
	cfg := peer.TLSConfig(cert, keyLog) // real avalanchego config
	if verify == "noop" {
		cfg.VerifyConnection = nil // decisive isolation cell
	}
	return cfg
}

func peerKeyType(cs tls.ConnectionState) string {
	if len(cs.PeerCertificates) == 0 {
		return "none"
	}
	switch cs.PeerCertificates[0].PublicKey.(type) {
	case *ecdsa.PublicKey:
		return "ecdsa"
	case *rsa.PublicKey:
		return "rsa"
	default:
		return "other"
	}
}

func report(conn *tls.Conn, err error) {
	if err != nil {
		emit(outcome{OK: false, Error: err.Error()})
		return
	}
	cs := conn.ConnectionState()
	emit(outcome{
		OK:          true,
		Version:     cs.Version,
		CipherSuite: cs.CipherSuite,
		PeerCertLen: len(cs.PeerCertificates),
		PeerKeyType: peerKeyType(cs),
	})
}

func main() {
	role := flag.String("role", "", "server|client")
	addr := flag.String("addr", "127.0.0.1:0", "host:port")
	verify := flag.String("verify", "staking", "staking|noop")
	keytype := flag.String("keytype", "ecdsa", "ecdsa|rsa")
	flag.Parse()

	var keyLog io.Writer
	if p := os.Getenv("SSLKEYLOGFILE"); p != "" {
		f, err := os.OpenFile(p, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o600)
		if err != nil {
			fatalf("open SSLKEYLOGFILE: %v", err)
		}
		defer f.Close()
		keyLog = f
	}

	cert := loadCert(*keytype)
	cfg := tlsConfig(cert, *verify, keyLog)

	switch *role {
	case "server":
		ln, err := tls.Listen("tcp", *addr, cfg)
		if err != nil {
			fatalf("listen: %v", err)
		}
		// Print the bound addr to stderr so the driver can read an ephemeral port.
		fmt.Fprintf(os.Stderr, "LISTENING %s\n", ln.Addr().String())
		raw, err := ln.Accept()
		if err != nil {
			fatalf("accept: %v", err)
		}
		conn := raw.(*tls.Conn)
		_ = conn.SetDeadline(time.Now().Add(10 * time.Second))
		report(conn, conn.Handshake())
		_ = conn.Close()
	case "client":
		raw, err := net.DialTimeout("tcp", *addr, 10*time.Second)
		if err != nil {
			fatalf("dial: %v", err)
		}
		conn := tls.Client(raw, cfg)
		_ = conn.SetDeadline(time.Now().Add(10 * time.Second))
		report(conn, conn.Handshake())
		_ = conn.Close()
	default:
		fatalf("unknown --role %q (want server|client)", *role)
	}
}
