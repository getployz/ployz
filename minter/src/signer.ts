import { createPrivateKey, sign, type KeyObject } from "node:crypto";
import { readFileSync } from "node:fs";
import { KMSClient, SignCommand } from "@aws-sdk/client-kms";

/** Signs the App JWT with RS256. The App's private key never leaves the signer. */
export interface Signer {
  sign(data: Uint8Array): Promise<Uint8Array>;
}

/** The deployed signer: the App key is KMS key material, and the minter can only ask KMS to sign. */
export class KmsSigner implements Signer {
  constructor(private readonly keyId: string, private readonly client = new KMSClient({})) {}

  async sign(data: Uint8Array): Promise<Uint8Array> {
    const { Signature } = await this.client.send(new SignCommand({
      KeyId: this.keyId,
      Message: data,
      MessageType: "RAW",
      SigningAlgorithm: "RSASSA_PKCS1_V1_5_SHA_256",
    }));
    if (!Signature) throw new Error("KMS returned no signature");
    return Signature;
  }
}

/** A PEM file on disk, for tests and lab lanes only. The Lambda entry refuses it. */
export class PemSigner implements Signer {
  private readonly key: KeyObject;

  constructor(pem: string) {
    this.key = createPrivateKey(pem);
  }

  static fromFile(path: string): PemSigner {
    return new PemSigner(readFileSync(path, "utf8"));
  }

  async sign(data: Uint8Array): Promise<Uint8Array> {
    return sign("sha256", data, this.key);
  }
}
