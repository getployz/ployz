var __defProp = Object.defineProperty;
var __name = (target, value) => __defProp(target, "name", { value, configurable: true });

// index.ts
import installScript from "./b14531d36e06a4eb41015381494366a19aa9f641-ployz.sh";
function response(body) {
  return new Response(body, {
    headers: {
      "content-type": "text/plain; charset=utf-8",
      "cache-control": "public, max-age=300",
      "x-content-type-options": "nosniff"
    }
  });
}
__name(response, "response");
var index_default = {
  fetch(request) {
    const { pathname } = new URL(request.url);
    if (pathname === "/" || pathname === "/ployz.sh") {
      return response(installScript);
    }
    return new Response("Not found\n", {
      status: 404,
      headers: {
        "content-type": "text/plain; charset=utf-8",
        "x-content-type-options": "nosniff"
      }
    });
  }
};
export {
  index_default as default
};
//# sourceMappingURL=index.js.map
