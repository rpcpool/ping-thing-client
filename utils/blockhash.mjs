import { safeRace } from "@solana/promises";
import { address } from "@solana/web3.js";

import { timeout } from "./misc.mjs";
import { rpc, rpcSubscriptions } from "./rpc.mjs";

const MAX_BLOCKHASH_FETCH_ATTEMPTS =
  process.env.MAX_BLOCKHASH_FETCH_ATTEMPTS || 5;
const RECENT_BLOCKHASHES_ADDRESS = address(
  "SysvarRecentB1ockHashes11111111111111111111"
);

async function getDifferenceBetweenSlotHeightAndBlockHeight() {
  const { absoluteSlot, blockHeight } = await rpc
    .getEpochInfo()
    .send({ abortSignal: AbortSignal.any([]) });
  return absoluteSlot - blockHeight;
}

function getLatestBlockhashesFromNotification(
  {
    context: { slot },
    value: {
      data: {
        parsed: {
          info: blockhashes,
        },
      },
    },
  },
  differenceBetweenSlotHeightAndBlockHeight
) {
  return blockhashes.map(({ blockhash }) => ({
    blockhash,
    lastValidBlockHeight:
      slot - differenceBetweenSlotHeightAndBlockHeight + 150n,
  }));
}

let resolveInitialLatestBlockhashes;
let latestBlockhashesPromise = new Promise((resolve) => {
  resolveInitialLatestBlockhashes = resolve;
});

let usedBlockhashes = new Set();

(async () => {
  let attempts = 0;
  while (true) {
    try {
      const [
        differenceBetweenSlotHeightAndBlockHeight,
        recentBlockhashesNotifications,
      ] = await safeRace([
        Promise.all([
          getDifferenceBetweenSlotHeightAndBlockHeight(),
          rpcSubscriptions
            .accountNotifications(RECENT_BLOCKHASHES_ADDRESS, {
              encoding: "jsonParsed",
            })
            .subscribe({ abortSignal: AbortSignal.any([]) }),
        ]),
        // If the RPC node fails to respond within 5 seconds, throw an error.
        timeout(5000),
      ]);
      // Iterate over the notificatons forever, constantly updating the `latestBlockhashes` cache.
      for await (const notification of recentBlockhashesNotifications) {
        const nextLatestBlockhashes = getLatestBlockhashesFromNotification(
          notification,
          differenceBetweenSlotHeightAndBlockHeight
        );
        const nextUsedBlockhashes = new Set();
        for (const { blockhash } of nextLatestBlockhashes) {
          if (usedBlockhashes.has(blockhash)) {
            nextUsedBlockhashes.add(blockhash)
          }
        }
        usedBlockhashes = nextUsedBlockhashes;
        attempts = 0;
        if (resolveInitialLatestBlockhashes) {
          resolveInitialLatestBlockhashes(nextLatestBlockhashes);
          resolveInitialLatestBlockhashes = undefined;
        } else {
          latestBlockhashesPromise = Promise.resolve(nextLatestBlockhashes);
        }
      }
    } catch (e) {
      if (e.message === "Timeout") {
        console.error(
          `${new Date().toISOString()} ERROR: Blockhash fetch operation timed out`
        );
      } else {
        console.error(e);
      }
      if (++attempts >= MAX_BLOCKHASH_FETCH_ATTEMPTS) {
        console.error(
          `${new Date().toISOString()} ERROR: Max attempts for fetching blockhash reached, exiting`
        );
        process.exit(0);
      }
    }
  }
})();

export async function getLatestBlockhash() {
  const latestBlockhashes = await latestBlockhashesPromise;
  const latestUnusedBlockhash = latestBlockhashes.find(
    ({ blockhash }) => !usedBlockhashes.has(blockhash),
  );
  if (!latestUnusedBlockhash) {
    console.error(
      `${new Date().toISOString()} ERROR: Ran out of unused blockhashes before the subscription could replenish them`,
    );
    process.exit(0);
  }
  usedBlockhashes.add(latestUnusedBlockhash.blockhash);
  return latestUnusedBlockhash;
}
