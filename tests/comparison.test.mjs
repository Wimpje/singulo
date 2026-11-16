import { test } from "node:test";
import assert from "node:assert/strict";
import { comparePictures } from "../src/lib/comparison.js";

const picture = (width, height, bytes) => ({ width, height, bytes });

test("recommends only at the twofold threshold", () => {
  const base = picture(100, 100, 100);
  assert.equal(comparePictures(picture(100, 100, 199), base).recommendation, null);
  assert.equal(comparePictures(picture(100, 100, 200), base).recommendation, 0);
  assert.equal(comparePictures(base, picture(200, 100, 100)).recommendation, 1);
  assert.equal(comparePictures(picture(199, 100, 100), base).recommendation, null);
});

test("does not recommend when resolution and size favor different pictures", () => {
  const result = comparePictures(picture(200, 100, 99), picture(100, 100, 100));
  assert.equal(result.resolution.substantiallyHigher, 0);
  assert.equal(result.size.higher, 1);
  assert.equal(result.recommendation, null);
  const both = comparePictures(picture(200, 100, 100), picture(100, 100, 200));
  assert.equal(both.resolution.substantiallyHigher, 0);
  assert.equal(both.size.substantiallyHigher, 1);
  assert.equal(both.recommendation, null);
});

test("ignores unavailable measurements and leaves ties unmarked", () => {
  const missing = comparePictures(picture(0, 0, 200), picture(100, 100, 100));
  assert.equal(missing.resolution.higher, null);
  assert.equal(missing.recommendation, 0);
  const unknownSize = comparePictures(picture(200, 100, 0), picture(100, 100, 100));
  assert.equal(unknownSize.recommendation, 0);
  const tied = comparePictures(picture(100, 100, 100), picture(100, 100, 100));
  assert.equal(tied.recommendation, null);
  assert.equal(tied.size.substantiallyHigher, null);
  assert.equal(tied.resolution.substantiallyHigher, null);
});
