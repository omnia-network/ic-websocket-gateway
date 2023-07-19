import "isomorphic-fetch";
import crypto from "isomorphic-webcrypto";
import util from 'util';

global.TextEncoder = util.TextEncoder;
// @ts-ignore
global.TextDecoder = util.TextDecoder;

// @ts-ignore
global.crypto.subtle = crypto.subtle;
