import installScript from "./ployz.txt";

function response(body: string) {
  return new Response(body, {
    headers: {
      "content-type": "text/plain; charset=utf-8",
      "content-disposition": 'attachment; filename="ployz.sh"',
      "cache-control": "public, max-age=300",
      "x-content-type-options": "nosniff",
    },
  });
}

export default {
  fetch(request: Request) {
    const { pathname } = new URL(request.url);

    if (pathname === "/" || pathname === "/ployz.sh" || pathname === "/ployz.txt") {
      return response(installScript);
    }

    return new Response("Not found\n", {
      status: 404,
      headers: {
        "content-type": "text/plain; charset=utf-8",
        "x-content-type-options": "nosniff",
      },
    });
  },
};
