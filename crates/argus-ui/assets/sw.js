self.addEventListener("push", (event) => {
  const data = event.data ? event.data.json() : {};
  const shown = self.registration.showNotification(data.title || "Argus", {
    body: data.body || "",
    tag: data.id,
    icon: "/icon-192.png",
    data: { url: data.url || "/" },
  });
  const origins = [self.location.origin, ...(data.receipts || [])];
  const receipts = data.id
    ? [...new Set(origins)].map((origin) =>
        Promise.resolve()
          .then(() => fetch(new URL("/api/v1/push/receipt/" + data.id, origin), { method: "POST", mode: "no-cors" }))
          .catch(() => {}),
      )
    : [];
  event.waitUntil(Promise.all([shown, ...receipts]));
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  event.waitUntil(self.clients.openWindow(event.notification.data.url));
});
