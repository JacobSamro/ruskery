import { createHash } from "node:crypto";
import { expect, type APIRequestContext } from "@playwright/test";

const sha = (b: Buffer) => "sha256:" + createHash("sha256").update(b).digest("hex");

/**
 * Push one image (config + a layer + a tag) into `org/repo` through the registry
 * API, mirroring the Rust e2e push helpers. Requires working object storage —
 * throws if a blob upload fails so callers can skip storage-dependent specs.
 */
export async function seedImage(
  request: APIRequestContext,
  org: string,
  repo: string,
  tag = "latest",
): Promise<void> {
  // 1. Mint a personal access token (dashboard cookie auth from storageState).
  const tokRes = await request.post("/api/v1/tokens", {
    data: { name: `seed-${Date.now()}` },
  });
  expect(tokRes.ok(), "create PAT").toBeTruthy();
  const pat = (await tokRes.json()).token as string;

  // 2. Exchange it for a registry bearer token.
  const basic = "Basic " + Buffer.from(`admin:${pat}`).toString("base64");
  const scope = `repository:${org}/${repo}:pull,push`;
  const tk = await request.get(
    `/v2/token?service=127.0.0.1&scope=${encodeURIComponent(scope)}`,
    { headers: { Authorization: basic } },
  );
  expect(tk.ok(), "exchange registry token").toBeTruthy();
  const auth = { Authorization: `Bearer ${(await tk.json()).token}` };
  const repoPath = `/v2/${org}/${repo}`;

  // 3. Monolithic blob uploads (POST .../uploads/?digest=).
  const pushBlob = async (bytes: Buffer): Promise<string> => {
    const digest = sha(bytes);
    const r = await request.post(
      `${repoPath}/blobs/uploads/?digest=${encodeURIComponent(digest)}`,
      { headers: { ...auth, "content-type": "application/octet-stream" }, data: bytes },
    );
    if (r.status() !== 201) {
      throw new Error(`blob upload failed (${r.status()}) — is object storage configured?`);
    }
    return digest;
  };
  const config = Buffer.from(`{"architecture":"amd64","os":"linux"}`);
  const layer = Buffer.from(`layer-${repo}-${Date.now()}`);
  const configDigest = await pushBlob(config);
  const layerDigest = await pushBlob(layer);

  // 4. Push the manifest under the tag.
  const manifest = {
    schemaVersion: 2,
    mediaType: "application/vnd.oci.image.manifest.v1+json",
    config: {
      mediaType: "application/vnd.oci.image.config.v1+json",
      digest: configDigest,
      size: config.length,
    },
    layers: [
      {
        mediaType: "application/vnd.oci.image.layer.v1.tar+gzip",
        digest: layerDigest,
        size: layer.length,
      },
    ],
  };
  const m = await request.put(`${repoPath}/manifests/${tag}`, {
    headers: { ...auth, "content-type": "application/vnd.oci.image.manifest.v1+json" },
    data: JSON.stringify(manifest),
  });
  if (m.status() !== 201) throw new Error(`manifest push failed (${m.status()})`);
}

/** Seed an image, returning false (instead of throwing) when storage is absent. */
export async function trySeedImage(
  request: APIRequestContext,
  org: string,
  repo: string,
  tag = "latest",
): Promise<boolean> {
  try {
    await seedImage(request, org, repo, tag);
    return true;
  } catch (e) {
    console.warn(`seedImage skipped: ${(e as Error).message}`);
    return false;
  }
}
