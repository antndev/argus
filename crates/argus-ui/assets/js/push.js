(() => {
  const bytes = (text) => {
    const base64 = text.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(text.length / 4) * 4, "=");
    return Uint8Array.from(atob(base64), (c) => c.charCodeAt(0));
  };
  const supported = () => "serviceWorker" in navigator && "PushManager" in window && "Notification" in window;

  window.argusPush = {
    state() {
      if (!supported()) return "unsupported";
      if (!window.isSecureContext) return "insecure";
      const standalone = window.matchMedia("(display-mode: standalone)").matches || navigator.standalone === true;
      const apple = /iPhone|iPad|iPod/.test(navigator.userAgent);
      if (apple && !standalone) return "install";
      return Notification.permission === "denied" ? "denied" : "ready";
    },
    async subscribe(key) {
      if ((await Notification.requestPermission()) !== "granted") throw new Error("Notifications were not allowed");
      const registration = await navigator.serviceWorker.register("/sw.js");
      await navigator.serviceWorker.ready;
      const previous = await registration.pushManager.getSubscription();
      if (previous) await previous.unsubscribe();
      const subscription = await registration.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: bytes(key) });
      const json = subscription.toJSON();
      return JSON.stringify({ endpoint: json.endpoint, p256dh: json.keys.p256dh, auth: json.keys.auth });
    },
  };

  if (supported() && window.isSecureContext) navigator.serviceWorker.register("/sw.js").catch(() => {});
})();
