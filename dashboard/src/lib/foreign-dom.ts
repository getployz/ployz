/**
 * Lets React keep working on a page the browser translated. Chrome's translate swaps each text node React rendered for
 * its own `<font>` elements, so React later removes a node that has left its parent, or inserts beside one, and the
 * DOM throws NotFoundError. That crash takes the whole page to its error screen (facebook/react#11538).
 *
 * A node already gone from its parent stays gone. A node whose neighbour has gone goes at the end of the parent, so it
 * still shows, perhaps out of place.
 */
export function tolerateForeignDom() {
  const remove = Node.prototype.removeChild;
  const insert = Node.prototype.insertBefore;
  Node.prototype.removeChild = function <T extends Node>(this: Node, child: T): T {
    return child.parentNode === this ? remove.call<Node, [T], T>(this, child) : child;
  };
  Node.prototype.insertBefore = function <T extends Node>(this: Node, node: T, before: Node | null): T {
    return insert.call<Node, [T, Node | null], T>(this, node, before !== null && before.parentNode !== this ? null : before);
  };
}
