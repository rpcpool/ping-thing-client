import { createSolanaRpc, createSolanaRpcSubscriptions_UNSTABLE } from "@solana/web3.js";

export const rpc = createSolanaRpc(
    process.env.RPC_ENDPOINT,
);
export const rpcSubscriptions = createSolanaRpcSubscriptions_UNSTABLE(
    process.env.WS_ENDPOINT,
);