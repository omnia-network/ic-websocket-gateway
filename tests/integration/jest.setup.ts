import "isomorphic-fetch";
import crypto from "isomorphic-webcrypto";
import util from 'util';

// @ts-ignore
global?.TextEncoder = util.TextEncoder;
// @ts-ignore
global?.TextDecoder = util.TextDecoder;

// @ts-ignore
global?.crypto?.subtle = crypto.subtle;

// for nodejs environment
Object.defineProperty(globalThis, 'crypto', {
  value: {
    getRandomValues: crypto.getRandomValues,
    subtle: crypto.subtle,
  }
});
