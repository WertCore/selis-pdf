// The worker half of the `service-worker` browser check (WEB.02).
//
// It is deliberately NOT apps/web/host's worker: that one is a large policy
// module already covered by 72 Node tests, and rebuilding it here would test
// the build, not the browser. This is the smallest worker that exercises the
// three engine behaviours WEB.02's status note called unverified:
//
//   install  -> a real precache write (addAll) into the engine's own Cache
//               Storage, not a double
//   activate -> a real clients.claim(), which is what makes the *first* page
//               load controlled rather than only the next navigation
//   fetch    -> a real respondWith, which the engine enforces

self.addEventListener("install", function (event) {
  // addAll is atomic: if the asset is missing the whole install fails and the
  // page never becomes controlled, which is the property under test.
  event.waitUntil(caches.open("selis-browser-harness-v1").then(function (cache) {
    return cache.addAll(["/asset.txt"]);
  }));
});

self.addEventListener("activate", function (event) {
  event.waitUntil(self.clients.claim());
});

self.addEventListener("fetch", function (event) {
  var url = new URL(event.request.url);
  if (url.pathname !== "/asset.txt") return;
  event.respondWith(
    caches.match("/asset.txt").then(function (hit) {
      return hit || fetch(event.request);
    }),
  );
});
