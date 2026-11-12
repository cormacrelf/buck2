self.onmessage = (e: MessageEvent<number>) => {
  self.postMessage(e.data * e.data);
};
