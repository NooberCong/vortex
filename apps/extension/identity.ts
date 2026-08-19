import { createHash } from "node:crypto";

/**
 * Who this extension is, before a store has an opinion.
 *
 * A Chromium extension's id is not a name someone picks — it is the first sixteen bytes of
 * the SHA-256 of its public key, with each nibble mapped onto `a`–`p`. For an *unpacked*
 * extension with no key, Chrome substitutes a hash of the absolute path it was loaded
 * from, which is why a development build has a different id on every machine and every
 * checkout.
 *
 * That is a problem here and not merely an annoyance. The native-messaging manifest pins
 * `allowed_origins` to exact ids and the platform forbids wildcards (01 §Security
 * boundaries), so an id nobody can predict is an id nobody can register — and the symptom
 * is the quiet one: capture goes passive and says nothing.
 *
 * So the build declares a key. The id below follows from it by arithmetic, is the same
 * everywhere, and is compiled into `vortex_setup::CHROMIUM_IDS` so that `vortexd
 * --register` pins it without being told. `tests/identity.test.ts` checks the two still
 * agree.
 *
 * The matching private key is *not* in this repository and is not needed to load, build or
 * register the extension — only to sign a CRX for self-hosted distribution, which is not
 * how Vortex ships. Regenerating the key would change the id and break every registration
 * already on disk, so it is a constant, not a rotation.
 */
export const EXTENSION_KEY =
  "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAxMzoNc+WVR82zO8z3fHsZzFLpGH0ua6O5YCAjtcG" +
  "8OywBkUCcqm/r5PzA55NWDm/hTM0sVBvYqdpvQpkQR0YTpEtJLKV2y5Wk+73Vf+JL3sqroBTUUXnrLD+fpAp" +
  "0bvFJ1wq4K103GNIOapUnb+euxoMaV05Vv94VddyAyayBAzoXtk1PW0X7j7YHD2bJ1suVu/pV1na1V+zrYUl" +
  "aKk8Z1OmRv+mOFtcvn6/OkGg5PV1sC4EgjXf1INlS/RumSvpTM0FAwRCs7hal57m6gtPqvCDhdmCmZIl09Ft" +
  "CX/5uz2EhZVxXGmIwAjs+O2esjA6ebHS2wNOU2SQs1Zj3w8eiQIDAQAB";

/**
 * The id Chromium will give a build carrying `key`.
 *
 * Derived rather than written down, so the pair cannot drift: `sha256(der)[0..16]`, hex,
 * then `0`–`f` shifted to `a`–`p`.
 */
export function extensionId(key: string = EXTENSION_KEY): string {
  const digest = createHash("sha256").update(Buffer.from(key, "base64")).digest();
  return [...digest.subarray(0, 16)]
    .flatMap((byte) => [byte >> 4, byte & 0xf])
    .map((nibble) => String.fromCharCode(97 + nibble))
    .join("");
}
