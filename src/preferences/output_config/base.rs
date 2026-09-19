// use glib::subclass::prelude::*;
// use gtk::glib::{self, object::IsA};

// // Interface struct (corresponds to C's GTypeInterface)
// #[derive(Clone, Copy)]
// #[repr(C)]
// pub struct CustomOutputConfigData {
//     parent: glib::gobject_ffi::GTypeInterface,
// }

// // Vtable
// #[derive(Clone, Copy)]
// #[repr(C)]
// pub struct CustomOutputConfigClass {
//     parent: glib::gobject_ffi::GTypeInterface,

//     pub generate_config: unsafe extern "C" fn (*mut CustomOutputConfigData) -> Vec<(String, String)>,
// }

// unsafe impl InterfaceStruct for CustomOutputConfigClass {
//     type Type = CustomOutputConfig;
// }

// // High-level wrapper for Rust-side code
// pub enum CustomOutputConfig {}

// #[glib::object_interface]
// impl ObjectInterface for CustomOutputConfig {
//     const NAME: &'static str = "MyStaticInterface";
//     type Interface = CustomOutputConfigClass;

//     // Optional: hook into interface initialization
//     fn interface_init(klass: &mut Self::Interface) {
//         // klass.generate_config =
//     }
// }

// // Trait for calling the method
// pub trait CustomOutputConfigExt: IsA<glib::Object> + 'static {
//     fn generate_config(&self) -> Vec<(String, String)> {
//         unsafe {
//             // Get the interface vtable from the object's instance pointer
//             let klass = *(self.as_ptr() as *mut *mut glib::gobject_ffi::GTypeClass);

//             // Look up the specific interface interface class structure
//             let iface = glib::gobject_ffi::g_type_interface_peek(
//                 klass as *mut _,
//                 <CustomOutputConfig as glib::types::StaticType>::static_type().into_glib(),
//             ) as *mut CustomOutputConfigClass;

//             assert!(!iface.is_null(), "Interface not implemented on this object");

//             // Dispatch to the function pointer
//             ((*iface).generate_config)(self.as_ptr() as *mut CustomOutputConfigData)
//         }
//     }
// }