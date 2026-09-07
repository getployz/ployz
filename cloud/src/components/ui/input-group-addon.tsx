"use client"

import * as React from "react"

import { cn } from "#/lib/utils.ts"
import {
  inputGroupAddonVariants,
  type InputGroupAddonVariants,
} from "#/components/ui/input-group-variants.ts"

function InputGroupAddon({
  className,
  align = "inline-start",
  ...props
}: React.ComponentProps<"div"> & InputGroupAddonVariants) {
  return (
    <div
      data-slot="input-group-addon"
      data-align={align}
      className={cn(inputGroupAddonVariants({ align }), className)}
      {...props}
    />
  )
}

export { InputGroupAddon }
