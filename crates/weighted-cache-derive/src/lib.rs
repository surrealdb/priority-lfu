//! A basic [`DeepSizeOf`](weighted_cache::DeepSizeOf) Derive implementation
//!
//! Mainly from `syn`'s [`heap_size` derive example][heap_size]
//!
//! [heap_size]: https://github.com/dtolnay/syn/commits/master/examples/heapsize/heapsize_derive/src/lib.rs

extern crate proc_macro;

use proc_macro2::TokenStream;
use quote::{quote, quote_spanned};
use syn::spanned::Spanned;
use syn::{
	Data, DeriveInput, Fields, GenericParam, Generics, Index, parse_macro_input, parse_quote,
};

/// Returns the path to the weighted_cache crate.
/// Uses `crate` when compiling the weighted-cache library itself, otherwise `::weighted_cache`.
fn crate_path() -> TokenStream {
	// CARGO_CRATE_NAME gives the actual crate being compiled (with hyphens as underscores).
	// For src/lib.rs of weighted-cache: CARGO_CRATE_NAME=weighted_cache
	// For tests/examples: CARGO_CRATE_NAME=integration_tests, etc.
	// For doctests: CARGO_CRATE_NAME=weighted_cache BUT we need to use external path.
	//
	// Detect doctests via UNSTABLE_RUSTDOC_TEST_PATH which rustdoc sets for doctest compilation.
	let is_weighted_cache_lib =
		std::env::var("CARGO_CRATE_NAME").is_ok_and(|name| name == "weighted_cache");
	let is_doctest = std::env::var_os("UNSTABLE_RUSTDOC_TEST_PATH").is_some()
		|| std::env::var_os("RUSTDOC").is_some();

	if is_weighted_cache_lib && !is_doctest {
		quote!(crate)
	} else {
		quote!(::weighted_cache)
	}
}

#[proc_macro_derive(DeepSizeOf)]
pub fn derive_deep_size(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
	// Parse the input tokens into a syntax tree.
	let input = parse_macro_input!(input as DeriveInput);

	// Used in the quasi-quotation below as `#name`.
	let name = input.ident;

	let krate = crate_path();

	// Add a bound `T: DeepSizeOf` to every type parameter T.
	let generics = add_trait_bounds(input.generics, &krate);
	let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

	// Generate an expression to sum up the size of each field.
	let sum = deepsize_sum(&input.data, &name, &krate);

	let expanded = quote! {
		// The generated impl.
		impl #impl_generics #krate::DeepSizeOf for #name #ty_generics #where_clause {
			fn deep_size_of_children(&self, context: &mut #krate::Context) -> usize {
				#sum
			}
		}
	};

	// Hand the output tokens back to the compiler.
	proc_macro::TokenStream::from(expanded)
}

// Add a bound `T: DeepSizeOf` to every type parameter T.
fn add_trait_bounds(mut generics: Generics, krate: &TokenStream) -> Generics {
	for param in &mut generics.params {
		if let GenericParam::Type(ref mut type_param) = *param {
			type_param.bounds.push(parse_quote!(#krate::DeepSizeOf));
		}
	}
	generics
}

fn match_fields(fields: &syn::Fields, krate: &TokenStream) -> TokenStream {
	match fields {
		Fields::Named(fields) => {
			let recurse = fields.named.iter().map(|f| {
				let name = &f.ident;
				quote_spanned! {f.span()=>
					#krate::DeepSizeOf::deep_size_of_children(&self.#name, context)
				}
			});
			quote! {
				0 #(+ #recurse)*
			}
		}
		Fields::Unnamed(fields) => {
			let recurse = fields.unnamed.iter().enumerate().map(|(i, f)| {
				let index = Index::from(i);
				quote_spanned! {f.span()=>
					#krate::DeepSizeOf::deep_size_of_children(&self.#index, context)
				}
			});
			quote! {
				0 #(+ #recurse)*
			}
		}
		Fields::Unit => {
			// Unit structs cannot own more than 0 bytes of memory.
			quote!(0)
		}
	}
}

fn match_enum_fields(fields: &syn::Fields, krate: &TokenStream) -> TokenStream {
	match fields {
		Fields::Named(fields) => {
			let recurse = fields.named.iter().map(|f| {
				let name = &f.ident;
				quote_spanned! {f.span()=>
					#krate::DeepSizeOf::deep_size_of_children(#name, context)
				}
			});
			quote! {
				0 #(+ #recurse)*
			}
		}
		Fields::Unnamed(fields) => {
			let recurse = fields.unnamed.iter().enumerate().map(|(i, f)| {
				let i = syn::Ident::new(&format!("_{}", i), proc_macro2::Span::call_site());
				quote_spanned! {f.span()=>
					#krate::DeepSizeOf::deep_size_of_children(#i, context)
				}
			});
			quote! {
				0 #(+ #recurse)*
			}
		}
		Fields::Unit => {
			// Unit structs cannot own more than 0 bytes of memory.
			quote!(0)
		}
	}
}

fn get_matcher(var: &syn::Variant) -> TokenStream {
	let matcher = match &var.fields {
		Fields::Unit => TokenStream::new(),
		Fields::Unnamed(fields) => {
			let fields: TokenStream = (0..fields.unnamed.len())
				.map(|n| {
					let i = syn::Ident::new(&format!("_{}", n), proc_macro2::Span::call_site());
					quote!(#i,)
				})
				.collect();
			quote!((#fields))
		}
		Fields::Named(fields) => {
			let fields: TokenStream = fields
				.named
				.iter()
				.map(|f| {
					let i = f.ident.as_ref().expect("Field identifier should be present");
					quote!(#i,)
				})
				.collect();
			quote!({#fields})
		}
	};

	quote!(#matcher)
}

/// Generate an expression to sum up the size of each field.
fn deepsize_sum(data: &Data, struct_name: &proc_macro2::Ident, krate: &TokenStream) -> TokenStream {
	match *data {
		Data::Struct(ref inner) => match_fields(&inner.fields, krate),
		Data::Enum(ref inner) => {
			let arms = inner.variants.iter().map(|var| {
				let matcher = get_matcher(var);
				let output = match_enum_fields(&var.fields, krate);
				let name = &var.ident;
				let ident = quote!(#struct_name::#name);
				quote!(#ident #matcher => #output,)
			});

			quote! {
				match self {
					#(#arms)*
					_ => 0 // This is needed for empty enums
				}
			}
		}
		Data::Union(_) => unimplemented!(),
	}
}
