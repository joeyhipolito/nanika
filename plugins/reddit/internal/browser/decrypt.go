package browser

import (
	"crypto/aes"
	"crypto/cipher"
	"crypto/hmac"
	"crypto/sha1"
	"fmt"
)

// decryptChromeValue decrypts a Chrome cookie value encrypted with AES-128-CBC.
// encrypted: raw bytes including the "v10" prefix
// password: Keychain password for "Chrome Safe Storage"
// metaVersion: Chrome cookie DB meta version (affects hash prefix)
func decryptChromeValue(encrypted []byte, password string, metaVersion int) (string, error) {
	if len(encrypted) < 3 {
		return "", fmt.Errorf("encrypted value too short (%d bytes)", len(encrypted))
	}

	// Strip "v10" prefix
	prefix := string(encrypted[:3])
	if prefix != "v10" {
		return "", fmt.Errorf("unexpected encryption version: %q (expected v10)", prefix)
	}
	ciphertext := encrypted[3:]

	// Derive key using PBKDF2-SHA1
	key := pbkdf2SHA1([]byte(password), []byte("saltysalt"), 1003, 16)

	// AES-128-CBC with IV = 16 space bytes (0x20)
	block, err := aes.NewCipher(key)
	if err != nil {
		return "", fmt.Errorf("creating AES cipher: %w", err)
	}

	if len(ciphertext)%aes.BlockSize != 0 {
		return "", fmt.Errorf("ciphertext length %d is not a multiple of AES block size", len(ciphertext))
	}

	iv := make([]byte, aes.BlockSize)
	for i := range iv {
		iv[i] = 0x20
	}

	mode := cipher.NewCBCDecrypter(block, iv)
	plaintext := make([]byte, len(ciphertext))
	mode.CryptBlocks(plaintext, ciphertext)

	// PKCS7 unpad
	plaintext, err = pkcs7Unpad(plaintext)
	if err != nil {
		return "", fmt.Errorf("PKCS7 unpad: %w", err)
	}

	// Chrome meta_version >= 24: first 32 bytes are SHA-256 hash prefix
	if metaVersion >= 24 && len(plaintext) > 32 {
		plaintext = plaintext[32:]
	}

	return string(plaintext), nil
}

// pbkdf2SHA1 implements PBKDF2 with HMAC-SHA1 per RFC 2898.
func pbkdf2SHA1(password, salt []byte, iterations, keyLen int) []byte {
	numBlocks := (keyLen + sha1.Size - 1) / sha1.Size
	dk := make([]byte, 0, numBlocks*sha1.Size)

	for block := 1; block <= numBlocks; block++ {
		dk = append(dk, pbkdf2Block(password, salt, iterations, block)...)
	}

	return dk[:keyLen]
}

func pbkdf2Block(password, salt []byte, iterations, blockNum int) []byte {
	mac := hmac.New(sha1.New, password)

	mac.Write(salt)
	mac.Write([]byte{byte(blockNum >> 24), byte(blockNum >> 16), byte(blockNum >> 8), byte(blockNum)})
	u := mac.Sum(nil)

	result := make([]byte, len(u))
	copy(result, u)

	for i := 2; i <= iterations; i++ {
		mac.Reset()
		mac.Write(u)
		u = mac.Sum(u[:0])
		for j := range result {
			result[j] ^= u[j]
		}
	}

	return result
}

// pkcs7Unpad removes PKCS#7 padding from data.
func pkcs7Unpad(data []byte) ([]byte, error) {
	if len(data) == 0 {
		return nil, fmt.Errorf("empty data")
	}

	padLen := int(data[len(data)-1])
	if padLen == 0 || padLen > aes.BlockSize || padLen > len(data) {
		return nil, fmt.Errorf("invalid padding length: %d", padLen)
	}

	for i := len(data) - padLen; i < len(data); i++ {
		if data[i] != byte(padLen) {
			return nil, fmt.Errorf("invalid padding byte at position %d", i)
		}
	}

	return data[:len(data)-padLen], nil
}
