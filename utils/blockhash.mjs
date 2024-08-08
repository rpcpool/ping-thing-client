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

function getLatestBlockhashFromNotification(
  {
    context: { slot },
    value: {
      data: {
        parsed: {
          info: [{ blockhash }],
        },
      },
    },
  },
  differenceBetweenSlotHeightAndBlockHeight
) {
  return {
    blockhash,
    lastValidBlockHeight:
      slot - differenceBetweenSlotHeightAndBlockHeight + 150n,
  };
}

let resolveInitialLatestBlockhash;
let latestBlockhashPromise = new Promise((resolve) => {
  resolveInitialLatestBlockhash = resolve;
});

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
      // Iterate over the notificatons forever, constantly updating the `latestBlockhash` cache.
      for await (const notification of recentBlockhashesNotifications) {
        const nextLatestBlockhash = getLatestBlockhashFromNotification(
          notification,
          differenceBetweenSlotHeightAndBlockHeight
        );
        attempts = 0;
        if (resolveInitialLatestBlockhash) {
          resolveInitialLatestBlockhash(nextLatestBlockhash);
          resolveInitialLatestBlockhash = undefined;
        } else {
          latestBlockhashPromise = Promise.resolve(nextLatestBlockhash);
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
  return await latestBlockhashPromise;
}
