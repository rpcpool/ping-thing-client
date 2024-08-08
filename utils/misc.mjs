export const sleep = async (duration) =>
  await new Promise((resolve) => setTimeout(resolve, duration));

export const timeout = async (duration) =>
  await new Promise((_, reject) => setTimeout(() => {
    reject(new Error('Timeout'));
  }, duration));