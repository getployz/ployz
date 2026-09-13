import { Button as ButtonPrimitive } from "@base-ui/react/button"

import { cn } from "#/lib/utils.ts"
import { buttonVariants, type ButtonVariants } from "#/components/ui/button-variants.ts"

function Button({
  className,
  variant = "default",
  size = "default",
  ...props
}: ButtonPrimitive.Props & ButtonVariants) {
  return (
    <ButtonPrimitive
      data-slot="button"
      className={cn(buttonVariants({ variant, size, className }))}
      {...props}
    />
  )
}

export { Button }
