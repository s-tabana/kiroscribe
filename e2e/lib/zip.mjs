// zip のファイルを読む（テストで、アプリが書き出した zip の中身を確かめるため）。
// 中央ディレクトリを読み、無圧縮（0）と Deflate（8）のファイルだけを展開する。

import zlib from "node:zlib";

/** zip のバイト列から、名前 → 中身（Buffer）の Map を返す */
export function readZip(buffer) {
  // 中央ディレクトリの終わりの記録（EOCD）を、末尾から探す
  let eocd = -1;
  for (let i = buffer.length - 22; i >= Math.max(0, buffer.length - 22 - 0xffff); i--) {
    if (buffer.readUInt32LE(i) === 0x06054b50) {
      eocd = i;
      break;
    }
  }
  if (eocd < 0) throw new Error("zip の終わりの記録が見つかりません");
  const count = buffer.readUInt16LE(eocd + 10);
  let offset = buffer.readUInt32LE(eocd + 16);
  const files = new Map();
  for (let n = 0; n < count; n++) {
    if (buffer.readUInt32LE(offset) !== 0x02014b50) throw new Error("zip の中央ディレクトリが壊れています");
    const method = buffer.readUInt16LE(offset + 10);
    const compressedSize = buffer.readUInt32LE(offset + 20);
    const nameLength = buffer.readUInt16LE(offset + 28);
    const extraLength = buffer.readUInt16LE(offset + 30);
    const commentLength = buffer.readUInt16LE(offset + 32);
    const localOffset = buffer.readUInt32LE(offset + 42);
    const name = buffer.toString("utf8", offset + 46, offset + 46 + nameLength);
    const localNameLength = buffer.readUInt16LE(localOffset + 26);
    const localExtraLength = buffer.readUInt16LE(localOffset + 28);
    const start = localOffset + 30 + localNameLength + localExtraLength;
    const data = buffer.subarray(start, start + compressedSize);
    if (method === 0) files.set(name, Buffer.from(data));
    else if (method === 8) files.set(name, zlib.inflateRawSync(data));
    else throw new Error(`${name}: 対応していない圧縮の方式です（${method}）`);
    offset += 46 + nameLength + extraLength + commentLength;
  }
  return files;
}
