// 官方固定版本的差分测试入口，仅使用合成请求与合成令牌
// Copyright 2026 OpenAI，Rust 测试修改适用随附非商业许可证
import { pathToFileURL } from 'node:url';
import { join } from 'node:path';
import { createCipheriv, createDecipheriv, randomBytes } from 'node:crypto';
const root = process.argv[2];
const { apiError } = await import(pathToFileURL(join(root, 'packages/local/dist/errors.js')));
const { listModels } = await import(pathToFileURL(join(root, 'packages/local/dist/models.js')));
const { streamResponse } = await import(pathToFileURL(join(root, 'packages/local/dist/responses.js')));
const { ConnectionStore } = await import(pathToFileURL(join(root, 'packages/local/dist/storage.js')));
let input = '';
for await (const chunk of process.stdin) input += chunk;
const results = [];
for (const scenario of JSON.parse(input)) {
  if (scenario.kind === 'storage') {
    const key = Buffer.from(scenario.key, 'base64');
    const provider = {
      id: 'test-aes-gcm', isAvailable: async () => true,
      encrypt(plaintext) {
        const nonce = randomBytes(12);
        const cipher = createCipheriv('aes-256-gcm', key, nonce);
        return Buffer.concat([nonce, cipher.update(plaintext, 'utf8'), cipher.final(), cipher.getAuthTag()]);
      },
      decrypt(ciphertext) {
        const bytes = Buffer.from(ciphertext);
        const cipher = createDecipheriv('aes-256-gcm', key, bytes.subarray(0, 12));
        cipher.setAuthTag(bytes.subarray(-16));
        return Buffer.concat([cipher.update(bytes.subarray(12, -16)), cipher.final()]).toString('utf8');
      },
    };
    const store = new ConnectionStore(scenario.directory, provider);
    results.push(await store.withLock(async () => {
      const saved = await store.read();
      const hostId = await store.getHostId();
      await store.write(saved);
      return { saved, hostId };
    }));
    continue;
  }
  if (scenario.kind === 'api') {
    results.push(apiError(scenario.body, scenario.status, scenario.requestId).toJSON());
    continue;
  }
  globalThis.fetch = async () => new Response(scenario.wire ?? JSON.stringify(scenario.body), {
    status: scenario.status ?? 200, headers: scenario.headers ?? {},
  });
  try {
    results.push(scenario.kind === 'models'
      ? await listModels('synthetic-oauth', new AbortController().signal)
      : await streamResponse('synthetic-oauth', { model: 'test-model', input: 'synthetic input' }, new AbortController().signal));
  } catch (error) {
    results.push(error.toJSON());
  }
}
process.stdout.write(JSON.stringify(results));
