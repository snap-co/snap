import React, { useEffect, useRef } from "react";
import { Link } from "@tanstack/react-router";

export type Section = "intakes" | "tickets" | "sessions";

export function Icon({ name }: { name: Section | "account" | "close" | "up" }) {
  return <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    {name === "intakes" && <><path d="M3 3h18v14H9l-6 4V3Z"/><path d="M7 8h10M7 12h6"/></>}
    {name === "tickets" && <><rect x="3" y="4" width="18" height="16" rx="2"/><path d="M7 9h10M7 14h6"/></>}
    {name === "sessions" && <><rect x="3" y="4" width="18" height="16" rx="2"/><path d="m7 9 3 3-3 3m7 0h3"/></>}
    {name === "account" && <><circle cx="12" cy="7" r="4"/><path d="M4 22v-3a8 8 0 0 1 16 0v3"/></>}
    {name === "close" && <path d="m6 6 12 12M18 6 6 18"/>}
    {name === "up" && <path d="m5 15 7-7 7 7"/>}
  </svg>;
}

export function ShellNavigation({ section, destinations, mobile = false }: { section: Section; destinations: Record<Section, string>; mobile?: boolean }) {
  return <nav className={mobile ? "mobile-navigation" : "desktop-navigation"} aria-label="Workspace">
    {(["intakes", "tickets", "sessions"] as const).map(item => <Link key={item} to={destinations[item]} aria-current={section === item ? "page" : undefined}>
      <Icon name={item}/><span>{item[0].toUpperCase() + item.slice(1)}</span>
    </Link>)}
  </nav>;
}

/** Native modal semantics contain focus, make the background inert and return
 * focus to the list trigger. The sidebar and drawer share the same list renderer. */
export function ListDrawer({ open, close, label, children }: { open: boolean; close: () => void; label: string; children: React.ReactNode }) {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const node = dialog.current!;
    if (open && !node.open) node.showModal();
    if (!open && node.open) node.close();
  }, [open]);
  useEffect(() => {
    const media = matchMedia("(min-width: 801px)");
    const changed = () => { if (media.matches) close(); };
    media.addEventListener("change", changed);
    return () => media.removeEventListener("change", changed);
  }, [close]);
  return <dialog id="work-list-drawer" ref={dialog} className="list-drawer" aria-labelledby="drawer-title" onCancel={close} onClose={close} onClick={event => { if (event.target === event.currentTarget) { const r = event.currentTarget.getBoundingClientRect(); if (event.clientX < r.left || event.clientX > r.right || event.clientY < r.top || event.clientY > r.bottom) close(); } }}>
    <div className="drawer-handle" aria-hidden="true"/>
    <div className="drawer-heading"><h2 id="drawer-title">{label}</h2><button className="quiet icon-button" aria-label="Close work list" autoFocus onClick={close}><Icon name="close"/></button></div>
    {children}
  </dialog>;
}
