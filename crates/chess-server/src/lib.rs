//! Authoritative multiplayer server for Terminal Chess. [`hub`] holds every
//! rule about rooms, seats, clocks, and persistence and never touches a socket.

pub mod hub;
