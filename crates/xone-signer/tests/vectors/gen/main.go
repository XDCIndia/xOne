package main

import (
	"crypto/aes"
	"crypto/cipher"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"

	"github.com/ethereum/go-ethereum/accounts/keystore"
	"github.com/ethereum/go-ethereum/crypto"
	"github.com/google/uuid"
	"golang.org/x/crypto/pbkdf2"
)

type vec struct {
	Name     string          `json:"name"`
	Password string          `json:"password"`
	Address  string          `json:"address"`
	Keystore json.RawMessage `json:"keystore"`
}

func write(dir, name, pw string, addr string, ks []byte) {
	b, _ := json.MarshalIndent(vec{name, pw, addr, ks}, "", "  ")
	if err := os.WriteFile(filepath.Join(dir, name+".json"), append(b, '\n'), 0644); err != nil {
		panic(err)
	}
}

func main() {
	dir := os.Args[1]
	for i, c := range []struct {
		name string
		n, p int
	}{{"scrypt-light", keystore.LightScryptN, keystore.LightScryptP}, {"scrypt-standard", keystore.StandardScryptN, keystore.StandardScryptP}} {
		k, _ := crypto.GenerateKey()
		id, _ := uuid.NewRandom()
		key := &keystore.Key{Id: id, Address: crypto.PubkeyToAddress(k.PublicKey), PrivateKey: k}
		pw := fmt.Sprintf("throwaway-password-%d", i)
		js, err := keystore.EncryptKey(key, pw, c.n, c.p)
		if err != nil {
			panic(err)
		}
		if _, err := keystore.DecryptKey(js, pw); err != nil {
			panic(err)
		}
		write(dir, c.name, pw, hex.EncodeToString(key.Address[:]), js)
		if i == 0 { // same file with the address field written in the XDC form (not MAC-covered)
			var m map[string]interface{}
			json.Unmarshal(js, &m)
			m["address"] = "xdc" + m["address"].(string)
			js2, _ := json.Marshal(m)
			if _, err := keystore.DecryptKey(js2, pw); err != nil {
				panic(err)
			}
			write(dir, "scrypt-light-xdc-address", pw, hex.EncodeToString(key.Address[:]), js2)
		}
	}
	// pbkdf2: geth has no encryptor; build the JSON the way EncryptDataV3 does, with the pbkdf2 KDF,
	// then check that the pinned geth DecryptKey accepts it.
	k, _ := crypto.GenerateKey()
	id, _ := uuid.NewRandom()
	addr := crypto.PubkeyToAddress(k.PublicKey)
	pw := "throwaway-password-pbkdf2"
	salt := make([]byte, 32)
	iv := make([]byte, 16)
	rand.Read(salt)
	rand.Read(iv)
	dk := pbkdf2.Key([]byte(pw), salt, 262144, 32, sha256.New)
	blk, _ := aes.NewCipher(dk[:16])
	ct := make([]byte, 32)
	cipher.NewCTR(blk, iv).XORKeyStream(ct, crypto.FromECDSA(k))
	mac := crypto.Keccak256(dk[16:32], ct)
	js, _ := json.Marshal(map[string]interface{}{
		"address": hex.EncodeToString(addr[:]), "id": id.String(), "version": 3,
		"crypto": map[string]interface{}{
			"cipher": "aes-128-ctr", "ciphertext": hex.EncodeToString(ct),
			"cipherparams": map[string]string{"iv": hex.EncodeToString(iv)},
			"kdf":          "pbkdf2",
			"kdfparams":    map[string]interface{}{"c": 262144, "dklen": 32, "prf": "hmac-sha256", "salt": hex.EncodeToString(salt)},
			"mac":          hex.EncodeToString(mac),
		},
	})
	if _, err := keystore.DecryptKey(js, pw); err != nil {
		panic(err)
	}
	write(dir, "pbkdf2", pw, hex.EncodeToString(addr[:]), js)
}
