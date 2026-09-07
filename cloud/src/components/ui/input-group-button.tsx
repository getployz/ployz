"use client"

import * as React from "react"

import { cn } from "#/lib/utils.ts"
import { Button } from "#/components/ui/button.tsx"
import {
  inputGroupButtonVariants,
  type InputGroupButtonVariants,
} from "#/components/ui/input-group-variants.ts"

function InputGroupButton({
  className,
  type = "button",
  variant = "ghost",
  size = "xs",
  ...props
}: Omit<React.ComponentProps<typeof Button>, "size" | "type"> &
  InputGroupButtonVariants & {
    type?: "button" | "submit" | "reset"
  }) {
  return (
    <Button
      type={type}
      data-size={size}
      variant={variant}
      className={cn(inputGroupButtonVariants({ size }), className)}
      {...props}
    />
  )
}

export { InputGroupButton }
