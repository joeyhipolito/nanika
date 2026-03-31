// shared-modules.ts — exposes React, ReactDOM, and dashboard UI primitives on
// window.__nanika_shared__ so dynamically-loaded plugin bundles can reference
// them without bundling their own copies.
//
// PATTERN: Initialize once on App mount via initSharedModules(). Plugin vite
// configs use vite-plugin-nanika-shared.ts to generate synthetic virtual
// modules that read from this global at runtime, making import() work without
// import maps or extra server configuration.
//
// Call order matters: initSharedModules() must run before any plugin bundle is
// loaded via import(). App.tsx useEffect on mount guarantees this for the
// normal flow.

import * as React from 'react'
import * as ReactDOM from 'react-dom'
import * as ReactDOMClient from 'react-dom/client'
import { Button, buttonVariants } from '../components/ui/button'
import { Badge, badgeVariants } from '../components/ui/badge'
import { Card, CardHeader, CardFooter, CardTitle, CardDescription, CardContent } from '../components/ui/card'
import { Tabs, TabsList, TabsTrigger, TabsContent } from '../components/ui/tabs'
import { cn } from './utils'
import * as wails from './wails'

export interface NanikaSharedUI {
  Button: typeof Button
  buttonVariants: typeof buttonVariants
  Badge: typeof Badge
  badgeVariants: typeof badgeVariants
  Card: typeof Card
  CardHeader: typeof CardHeader
  CardFooter: typeof CardFooter
  CardTitle: typeof CardTitle
  CardDescription: typeof CardDescription
  CardContent: typeof CardContent
  Tabs: typeof Tabs
  TabsList: typeof TabsList
  TabsTrigger: typeof TabsTrigger
  TabsContent: typeof TabsContent
  cn: typeof cn
}

export interface NanikaShared {
  react: typeof React
  reactDom: typeof ReactDOM
  reactDomClient: typeof ReactDOMClient
  ui: NanikaSharedUI
  wails: typeof wails
}

declare global {
  interface Window {
    __nanika_shared__: NanikaShared
  }
}

export function initSharedModules(): void {
  console.log('[shared-modules] initSharedModules() called')
  if (typeof window === 'undefined') {
    console.log('[shared-modules] window is undefined, skipping')
    return
  }
  // Already initialized — idempotent, safe to call multiple times.
  if (window.__nanika_shared__ != null) {
    console.log('[shared-modules] already initialized, skipping')
    return
  }

  console.log('[shared-modules] injecting window.__nanika_shared__')
  window.__nanika_shared__ = {
    react: React,
    reactDom: ReactDOM,
    reactDomClient: ReactDOMClient,
    ui: {
      Button,
      buttonVariants,
      Badge,
      badgeVariants,
      Card,
      CardHeader,
      CardFooter,
      CardTitle,
      CardDescription,
      CardContent,
      Tabs,
      TabsList,
      TabsTrigger,
      TabsContent,
      cn,
    },
    wails,
  }
  console.log('[shared-modules] window.__nanika_shared__ injected successfully, keys:', Object.keys(window.__nanika_shared__))
}
