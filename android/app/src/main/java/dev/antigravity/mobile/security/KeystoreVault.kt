package dev.antigravity.mobile.security

import android.content.Context
import android.os.Build
import android.security.KeyPairGeneratorSpec
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.io.File
import java.math.BigInteger
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.SecureRandom
import java.util.Calendar
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import javax.security.auth.x500.X500Principal

data class VaultKeyMaterial(
    val keyBytes: ByteArray,
    val keystoreBacked: Boolean
)

object KeystoreVault {
    private const val ANDROID_KEYSTORE = "AndroidKeyStore"
    private const val ALIAS_AES = "agy_mobile_vault_aes_v1"
    private const val ALIAS_RSA = "agy_mobile_vault_rsa_v1"

    fun loadOrCreateVaultKey(context: Context): VaultKeyMaterial {
        val wrappedFile = File(context.filesDir, "vault_key.wrapped")
        return try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
                loadOrCreateApi23AesGcm(wrappedFile)
            } else {
                loadOrCreateApi21RsaWrap(context, wrappedFile)
            }
        } catch (_: Throwable) {
            // Fallback on broken OEM Android 5.x keystores while keeping 0600 app-sandbox isolation.
            loadOrCreateAppSandboxFallback(File(context.filesDir, "vault_key.local"))
        }
    }

    private fun loadOrCreateApi23AesGcm(wrappedFile: File): VaultKeyMaterial {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.M) {
            throw IllegalStateException("API 23 required")
        }
        val ks = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        val secretKey: SecretKey = if (ks.containsAlias(ALIAS_AES)) {
            (ks.getEntry(ALIAS_AES, null) as KeyStore.SecretKeyEntry).secretKey
        } else {
            val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEYSTORE)
            val spec = KeyGenParameterSpec.Builder(
                ALIAS_AES,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT
            )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build()
            gen.init(spec)
            gen.generateKey()
        }

        if (wrappedFile.isFile) {
            val blob = wrappedFile.readBytes()
            if (blob.size > 13) {
                val ivLen = blob[0].toInt() and 0xFF
                val iv = blob.copyOfRange(1, 1 + ivLen)
                val ciphertext = blob.copyOfRange(1 + ivLen, blob.size)
                val cipher = Cipher.getInstance("AES/GCM/NoPadding")
                cipher.init(Cipher.DECRYPT_MODE, secretKey, GCMParameterSpec(128, iv))
                val raw = cipher.doFinal(ciphertext)
                if (raw.size == 32) {
                    return VaultKeyMaterial(raw, true)
                }
            }
        }

        val fresh = ByteArray(32).also { SecureRandom().nextBytes(it) }
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, secretKey)
        val iv = cipher.iv
        val enc = cipher.doFinal(fresh)
        val out = ByteArray(1 + iv.size + enc.size)
        out[0] = iv.size.toByte()
        System.arraycopy(iv, 0, out, 1, iv.size)
        System.arraycopy(enc, 0, out, 1 + iv.size, enc.size)
        wrappedFile.writeBytes(out)
        return VaultKeyMaterial(fresh, true)
    }

    @Suppress("DEPRECATION")
    private fun loadOrCreateApi21RsaWrap(context: Context, wrappedFile: File): VaultKeyMaterial {
        val ks = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        if (!ks.containsAlias(ALIAS_RSA)) {
            val start = Calendar.getInstance()
            val end = Calendar.getInstance().apply { add(Calendar.YEAR, 25) }
            val spec = KeyPairGeneratorSpec.Builder(context)
                .setAlias(ALIAS_RSA)
                .setSubject(X500Principal("CN=AGY Mobile Vault"))
                .setSerialNumber(BigInteger.ONE)
                .setStartDate(start.time)
                .setEndDate(end.time)
                .build()
            val kpg = KeyPairGenerator.getInstance("RSA", ANDROID_KEYSTORE)
            kpg.initialize(spec)
            kpg.generateKeyPair()
        }

        val entry = ks.getEntry(ALIAS_RSA, null) as KeyStore.PrivateKeyEntry
        if (wrappedFile.isFile) {
            val cipher = Cipher.getInstance("RSA/ECB/PKCS1Padding")
            cipher.init(Cipher.DECRYPT_MODE, entry.privateKey)
            val raw = cipher.doFinal(wrappedFile.readBytes())
            if (raw.size == 32) {
                return VaultKeyMaterial(raw, true)
            }
        }

        val fresh = ByteArray(32).also { SecureRandom().nextBytes(it) }
        val cipher = Cipher.getInstance("RSA/ECB/PKCS1Padding")
        cipher.init(Cipher.ENCRYPT_MODE, entry.certificate.publicKey)
        wrappedFile.writeBytes(cipher.doFinal(fresh))
        return VaultKeyMaterial(fresh, true)
    }

    private fun loadOrCreateAppSandboxFallback(file: File): VaultKeyMaterial {
        if (file.isFile) {
            val existing = file.readBytes()
            if (existing.size == 32) {
                return VaultKeyMaterial(existing, false)
            }
        }
        val fresh = ByteArray(32).also { SecureRandom().nextBytes(it) }
        file.writeBytes(fresh)
        file.setReadable(false, false)
        file.setReadable(true, true)
        return VaultKeyMaterial(fresh, false)
    }
}
